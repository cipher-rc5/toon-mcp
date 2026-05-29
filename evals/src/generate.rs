//! LLM-driven synthetic corpus generation.
//!
//! gemma is asked to emit raw payloads (no prose, no fences) across a matrix of
//! formats x shapes x domains x sizes. Each candidate is sanitised and then
//! validated by the *real* core parser; only payloads that parse are kept, so
//! downstream stages never choke on malformed model output.

use crate::corpus::{CorpusItem, JsonlAppender};
use crate::llm::LlmClient;
use crate::toonkit::parse_to_value;
use anyhow::Result;
use std::path::Path;

#[derive(Clone, Copy)]
struct ShapeRecipe {
    /// Maps to `CorpusItem::intended_shape`.
    name: &'static str,
    /// Instruction fragment describing the structure to produce.
    instruction: &'static str,
}

const SHAPES: &[ShapeRecipe] = &[
    ShapeRecipe {
        name: "tabular",
        instruction: "a flat array of objects where EVERY object has the SAME set of scalar keys (uniform schema, no nesting). This is the bread-and-butter case for tabular compression.",
    },
    ShapeRecipe {
        name: "fold_chain",
        instruction: "a deeply nested object: several levels of single-key wrapper objects (e.g. data->result->items) that finally contain a uniform array of objects at the leaf. Designed to exercise key folding.",
    },
    ShapeRecipe {
        name: "primitive_array",
        instruction: "a large flat array of primitive scalars only (numbers or short strings), no objects.",
    },
    ShapeRecipe {
        name: "mixed",
        instruction: "an array of objects with IRREGULAR schemas: objects have differing, partially-overlapping key sets and some nested sub-objects. Realistically messy.",
    },
    ShapeRecipe {
        name: "pass_through",
        instruction: "data that should NOT benefit from tabular compression: e.g. a single object with a few keys, or an array of only 2-3 dissimilar items. Keep it small and irregular.",
    },
];

const DOMAINS: &[&str] = &[
    "ecommerce order line items",
    "iot sensor telemetry readings",
    "user account directory records",
    "financial transaction ledger entries",
    "github-style issue tracker tickets",
    "flight schedule departures",
    "clinical lab result panels",
    "server access log events",
    "product catalog inventory",
    "sports match statistics",
    "geographic place-of-interest listings",
    "music streaming play history",
];

/// (format, shapes-that-make-sense-for-it, approx record counts to request)
const FORMAT_PLAN: &[(&str, &[&str], &[u32])] = &[
    (
        "json",
        &[
            "tabular",
            "fold_chain",
            "primitive_array",
            "mixed",
            "pass_through",
        ],
        &[12, 40, 120],
    ),
    ("jsonl", &["tabular", "mixed"], &[15, 60, 150]),
    ("csv", &["tabular"], &[20, 80, 200]),
    ("tsv", &["tabular"], &[20, 80]),
];

fn system_prompt(format: &str) -> String {
    format!(
        "You are a synthetic data generator. Output ONLY raw {fmt} data and nothing else: \
         no explanation, no commentary, no markdown code fences, no leading or trailing text. \
         The very first character of your reply must be the first character of the {fmt} payload. \
         Make the data realistic and varied with plausible values.",
        fmt = format.to_uppercase()
    )
}

fn user_prompt(format: &str, shape: &ShapeRecipe, domain: &str, records: u32) -> String {
    let fmt_hint = match format {
        "json" => "Produce a single valid JSON document.",
        "jsonl" => {
            "Produce JSONL: one valid JSON object per line, newline-separated, no enclosing array."
        }
        "csv" => "Produce CSV with a header row followed by data rows.",
        "tsv" => "Produce TSV (tab-separated) with a header row followed by data rows.",
        _ => "",
    };
    format!(
        "Domain: {domain}.\nStructure: {structure}\nTarget size: about {records} records/rows.\n{fmt_hint}",
        structure = shape.instruction,
    )
}

/// Strip common LLM wrapping: leading prose, ```fences```, trailing chatter.
fn sanitize(raw: &str, format: &str) -> String {
    let mut s = raw.trim();
    // Strip a fenced block if present, keeping its contents.
    if let Some(start) = s.find("```") {
        let after = &s[start + 3..];
        // drop an optional language tag on the same line
        let after = after.split_once('\n').map(|x| x.1).unwrap_or(after);
        if let Some(end) = after.find("```") {
            s = after[..end].trim();
        } else {
            s = after.trim();
        }
    }
    // For JSON, clip to the outermost bracket pair to drop stray prose.
    if format == "json"
        && let (Some(lo), Some(hi)) = (s.find(['{', '[']), s.rfind(['}', ']']))
        && hi >= lo
    {
        s = &s[lo..=hi];
    }
    s.to_string()
}

pub struct GenConfig {
    pub temperature: f32,
    pub max_tokens: u32,
    /// Per (format, shape, size) cell, how many distinct payloads to request.
    pub per_cell: usize,
    /// Retries when a payload fails to parse.
    pub max_retries: usize,
}

impl Default for GenConfig {
    fn default() -> Self {
        Self {
            temperature: 0.9,
            max_tokens: 4096,
            per_cell: 1,
            max_retries: 2,
        }
    }
}

/// Generate the corpus, appending each accepted item to `out` as it lands.
/// Returns (accepted, attempted).
pub fn generate(client: &LlmClient, cfg: &GenConfig, out: &Path) -> Result<(usize, usize)> {
    let mut sink = JsonlAppender::create(out)?;
    let mut accepted = 0usize;
    let mut attempted = 0usize;
    let mut seq = 0usize;

    for (format, shape_names, sizes) in FORMAT_PLAN {
        for shape in SHAPES.iter().filter(|s| shape_names.contains(&s.name)) {
            for &records in *sizes {
                for cell in 0..cfg.per_cell {
                    let domain = DOMAINS[seq % DOMAINS.len()];
                    seq += 1;
                    let sys = system_prompt(format);
                    let usr = user_prompt(format, shape, domain, records);

                    let mut kept = false;
                    for attempt in 0..=cfg.max_retries {
                        attempted += 1;
                        let raw = match client.chat(&sys, &usr, cfg.temperature, cfg.max_tokens) {
                            Ok(r) => r,
                            Err(e) => {
                                eprintln!("  ! chat error ({format}/{}): {e}", shape.name);
                                continue;
                            }
                        };
                        let payload = sanitize(&raw, format);
                        if payload.is_empty() {
                            continue;
                        }
                        // Validate against the real parser.
                        if parse_to_value(format, &payload, true).is_err() {
                            if attempt == cfg.max_retries {
                                eprintln!(
                                    "  ! dropped unparseable {format}/{} ({domain})",
                                    shape.name
                                );
                            }
                            continue;
                        }
                        let id = format!("{format}-{}-{records}-{cell}-{seq}", shape.name);
                        let item = CorpusItem {
                            id,
                            format: (*format).to_string(),
                            category: domain.to_string(),
                            intended_shape: shape.name.to_string(),
                            bytes: payload.len(),
                            payload,
                        };
                        sink.push(&item)?;
                        accepted += 1;
                        kept = true;
                        println!(
                            "  + {format:5} {:14} ~{records:>4} rec  {} B  [{domain}]",
                            shape.name, item.bytes
                        );
                        break;
                    }
                    if !kept {
                        eprintln!("  - gave up on {format}/{} ({domain})", shape.name);
                    }
                }
            }
        }
    }
    Ok((accepted, attempted))
}
