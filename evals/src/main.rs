//! toon-evals — internal evaluation harness for toon-mcp compression quality.
//!
//! Stages (run individually or via `all`):
//!   generate    use gemma to synthesize a corpus of structured payloads
//!   pipeline    run the corpus through toon-mcp-core; measure bytes/tokens/
//!               round-trip/classification
//!   comprehend  ask gemma retrieval questions over JSON vs TOON; measure parity
//!   report      aggregate results into report.md + summary.json
//!
//! Config via env: TOON_EVAL_BASE_URL (default http://localhost:8080),
//!                 TOON_EVAL_MODEL    (default local-model). llama-server
//!                 serves whatever model is loaded regardless of this name.

mod comprehension;
mod corpus;
mod generate;
mod llm;
mod pipeline;
mod report;
mod toonkit;

use anyhow::{Result, bail};
use corpus::{CorpusItem, read_jsonl, write_jsonl};
use llm::LlmClient;
use pipeline::PipelineResult;
use std::path::PathBuf;
use toon_mcp_core::CompressConfig;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn results_dir() -> PathBuf {
    PathBuf::from(env_or("TOON_EVAL_RESULTS", "results"))
}

fn client() -> LlmClient {
    LlmClient::new(
        env_or("TOON_EVAL_BASE_URL", "http://localhost:8080"),
        env_or("TOON_EVAL_MODEL", "local-model"),
    )
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("help");
    let rest = if args.is_empty() { &[][..] } else { &args[1..] };
    let dir = results_dir();

    match cmd {
        "generate" => cmd_generate(rest, &dir),
        "pipeline" => cmd_pipeline(rest, &dir),
        "comprehend" => cmd_comprehend(rest, &dir),
        "report" => cmd_report(&dir),
        "all" => {
            cmd_generate(rest, &dir)?;
            cmd_pipeline(rest, &dir)?;
            cmd_comprehend(rest, &dir)?;
            cmd_report(&dir)
        }
        _ => {
            eprintln!(
                "usage: toon-evals <generate|pipeline|comprehend|report|all> [--per-cell N] [--max-items N]\n\
                 env: TOON_EVAL_BASE_URL, TOON_EVAL_MODEL, TOON_EVAL_RESULTS"
            );
            Ok(())
        }
    }
}

fn cmd_generate(args: &[String], dir: &std::path::Path) -> Result<()> {
    let c = client();
    if let Err(e) = c.ping() {
        bail!("{e}\nStart the server first (see evals/README.md).");
    }
    let mut cfg = generate::GenConfig::default();
    if let Some(n) = flag(args, "--per-cell").and_then(|s| s.parse().ok()) {
        cfg.per_cell = n;
    }
    let out = dir.join("corpus.jsonl");
    println!("Generating corpus → {} ...", out.display());
    let (accepted, attempted) = generate::generate(&c, &cfg, &out)?;
    println!("\nGenerated {accepted} items ({attempted} attempts).");
    Ok(())
}

fn cmd_pipeline(args: &[String], dir: &std::path::Path) -> Result<()> {
    let corpus_path = dir.join("corpus.jsonl");
    let mut items: Vec<CorpusItem> = read_jsonl(&corpus_path)?;
    if let Some(n) = flag(args, "--max-items").and_then(|s| s.parse().ok()) {
        items.truncate(n);
    }
    // Tokenizer is optional; skip cleanly if the server is down.
    let c = client();
    let tok = c.ping().is_ok().then_some(&c);
    if tok.is_none() {
        eprintln!("(server unreachable — token-savings columns will be empty)");
    }

    let config = CompressConfig::default();
    let mut results = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let r = pipeline::evaluate_item(item, &config, tok);
        if (i + 1) % 10 == 0 || i + 1 == items.len() {
            println!("  pipeline {}/{}", i + 1, items.len());
        }
        results.push(r);
    }
    let out = dir.join("pipeline.jsonl");
    write_jsonl(&out, &results)?;
    println!("Wrote {} → {}", results.len(), out.display());
    Ok(())
}

fn cmd_comprehend(args: &[String], dir: &std::path::Path) -> Result<()> {
    let corpus_path = dir.join("corpus.jsonl");
    let mut items: Vec<CorpusItem> = read_jsonl(&corpus_path)?;
    if let Some(n) = flag(args, "--max-items").and_then(|s| s.parse().ok()) {
        items.truncate(n);
    }
    let c = client();
    if let Err(e) = c.ping() {
        bail!("{e}\ncomprehend requires a live server.");
    }
    let cfg = comprehension::ComprehendConfig::default();
    let mut all = Vec::new();
    for (i, item) in items.iter().enumerate() {
        match comprehension::evaluate_item(&c, item, &cfg) {
            Ok(rs) => all.extend(rs),
            Err(e) => eprintln!("  ! comprehend {} error: {e}", item.id),
        }
        if (i + 1) % 5 == 0 || i + 1 == items.len() {
            println!(
                "  comprehend {}/{} ({} Q so far)",
                i + 1,
                items.len(),
                all.len()
            );
        }
    }
    let out = dir.join("comprehension.jsonl");
    write_jsonl(&out, &all)?;
    println!("Wrote {} questions → {}", all.len(), out.display());
    Ok(())
}

fn cmd_report(dir: &std::path::Path) -> Result<()> {
    let pipeline: Vec<PipelineResult> = read_jsonl(&dir.join("pipeline.jsonl"))?;
    let comprehension = read_jsonl(&dir.join("comprehension.jsonl")).unwrap_or_default();
    report::build(&pipeline, &comprehension, dir)
}
