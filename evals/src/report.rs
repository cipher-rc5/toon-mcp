//! Aggregates pipeline + comprehension results into a human-readable Markdown
//! report and a machine-readable `summary.json`.

use crate::comprehension::ComprehensionResult;
use crate::pipeline::PipelineResult;
use anyhow::Result;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

fn mean(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        0.0
    } else {
        xs.iter().sum::<f64>() / xs.len() as f64
    }
}

#[derive(Default)]
struct Bucket {
    n: usize,
    compressed: usize,
    pipeline_savings: Vec<f64>,
    byte_savings: Vec<f64>,
    token_savings: Vec<f64>,
}

impl Bucket {
    fn add(&mut self, r: &PipelineResult) {
        self.n += 1;
        if r.compressed {
            self.compressed += 1;
        }
        if let Some(v) = r.pipeline_byte_savings_pct {
            self.pipeline_savings.push(v);
        }
        if let Some(v) = r.byte_savings_pct {
            self.byte_savings.push(v);
        }
        if let Some(v) = r.token_savings_pct {
            self.token_savings.push(v);
        }
    }
    fn row(&self, label: &str) -> String {
        format!(
            "| {label} | {} | {:.0}% | {:.1}% | {:.1}% | {:.1}% |",
            self.n,
            100.0 * self.compressed as f64 / self.n.max(1) as f64,
            mean(&self.pipeline_savings),
            mean(&self.byte_savings),
            mean(&self.token_savings),
        )
    }
}

pub fn build(
    pipeline: &[PipelineResult],
    comprehension: &[ComprehensionResult],
    out_dir: &Path,
) -> Result<()> {
    fs::create_dir_all(out_dir)?;
    let mut md = String::new();
    writeln!(md, "# toon-mcp evaluation report\n")?;
    writeln!(
        md,
        "_Items: {} pipeline, {} comprehension Q_\n",
        pipeline.len(),
        comprehension.len()
    )?;

    // ---- Overall + per-format / per-shape savings ----
    let mut overall = Bucket::default();
    let mut by_format: BTreeMap<String, Bucket> = BTreeMap::new();
    let mut by_shape: BTreeMap<String, Bucket> = BTreeMap::new();
    for r in pipeline {
        overall.add(r);
        by_format.entry(r.format.clone()).or_default().add(r);
        by_shape.entry(r.intended_shape.clone()).or_default().add(r);
    }

    writeln!(md, "## Savings\n")?;
    writeln!(
        md,
        "Columns: count · compress-rate · pipeline byte-savings (gated, compressed only) · self-encoded byte-savings · **token-savings (exact, model tokenizer)**.\n"
    )?;
    writeln!(
        md,
        "| group | n | compress% | pipe-byte% | byte% | token% |"
    )?;
    writeln!(md, "|---|--:|--:|--:|--:|--:|")?;
    writeln!(md, "{}", overall.row("**ALL**"))?;
    for (k, b) in &by_format {
        writeln!(md, "{}", b.row(&format!("fmt:{k}")))?;
    }
    for (k, b) in &by_shape {
        writeln!(md, "{}", b.row(&format!("shape:{k}")))?;
    }

    writeln!(
        md,
        "\n> **Token vs byte:** mean token-savings {:.1}% vs mean byte-savings {:.1}% (delta {:+.1} pts). \
         Byte ratio is what the pipeline gates on; token savings is the real context-window win.\n",
        mean(&overall.token_savings),
        mean(&overall.byte_savings),
        mean(&overall.token_savings) - mean(&overall.byte_savings),
    )?;

    // ---- Pass-through reasons ----
    let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
    for r in pipeline {
        if let Some(reason) = &r.pass_reason {
            *reasons.entry(reason.clone()).or_default() += 1;
        }
    }
    writeln!(md, "## Pass-through reasons\n")?;
    writeln!(md, "| reason | count |\n|---|--:|")?;
    for (k, v) in &reasons {
        writeln!(md, "| {k} | {v} |")?;
    }
    if reasons.is_empty() {
        writeln!(md, "| (none — all compressed) | 0 |")?;
    }

    // ---- Round-trip fidelity ----
    let rt: Vec<&PipelineResult> = pipeline
        .iter()
        .filter(|r| r.roundtrip_ok.is_some())
        .collect();
    let rt_fail: Vec<&&PipelineResult> = rt
        .iter()
        .filter(|r| r.roundtrip_ok == Some(false))
        .collect();
    writeln!(md, "\n## Round-trip fidelity (encode → decode)\n")?;
    writeln!(
        md,
        "Tested: {} · Lossless: {} · **Failures: {}**\n",
        rt.len(),
        rt.len() - rt_fail.len(),
        rt_fail.len()
    )?;
    for r in rt_fail.iter().take(25) {
        writeln!(
            md,
            "- `{}` ({}/{}) — {}",
            r.id,
            r.format,
            r.intended_shape,
            r.roundtrip_detail.as_deref().unwrap_or("")
        )?;
    }

    // ---- Classification accuracy + confusion ----
    let graded: Vec<&PipelineResult> = pipeline
        .iter()
        .filter(|r| r.shape_correct.is_some())
        .collect();
    let correct = graded
        .iter()
        .filter(|r| r.shape_correct == Some(true))
        .count();
    writeln!(md, "\n## Classification (intended vs classifier)\n")?;
    writeln!(
        md,
        "Accuracy: {}/{} = {:.0}%\n",
        correct,
        graded.len(),
        100.0 * correct as f64 / graded.len().max(1) as f64
    )?;
    let mut confusion: BTreeMap<(String, String), usize> = BTreeMap::new();
    for r in &graded {
        let actual = r.actual_shape.clone().unwrap_or_else(|| "?".into());
        *confusion
            .entry((r.intended_shape.clone(), actual))
            .or_default() += 1;
    }
    writeln!(md, "| intended → actual | count |\n|---|--:|")?;
    for ((i, a), c) in &confusion {
        let mark = if i == a { "" } else { "  ⚠️" };
        writeln!(md, "| {i} → {a}{mark} | {c} |")?;
    }

    // ---- Comprehension parity ----
    writeln!(md, "\n## Comprehension parity (JSON vs TOON answers)\n")?;
    if comprehension.is_empty() {
        writeln!(md, "_No comprehension questions run._\n")?;
    } else {
        let mut by_q: BTreeMap<String, (usize, usize, usize)> = BTreeMap::new();
        let (mut tot, mut cj, mut ct) = (0usize, 0usize, 0usize);
        for r in comprehension {
            let e = by_q.entry(r.qtype.clone()).or_default();
            e.0 += 1;
            e.1 += r.correct_json as usize;
            e.2 += r.correct_toon as usize;
            tot += 1;
            cj += r.correct_json as usize;
            ct += r.correct_toon as usize;
        }
        writeln!(md, "| question | n | acc(JSON) | acc(TOON) | parity Δ |")?;
        writeln!(md, "|---|--:|--:|--:|--:|")?;
        let pct = |a: usize, n: usize| 100.0 * a as f64 / n.max(1) as f64;
        for (q, (n, j, t)) in &by_q {
            writeln!(
                md,
                "| {q} | {n} | {:.0}% | {:.0}% | {:+.0} pts |",
                pct(*j, *n),
                pct(*t, *n),
                pct(*t, *n) - pct(*j, *n)
            )?;
        }
        writeln!(
            md,
            "| **ALL** | {tot} | {:.0}% | {:.0}% | {:+.0} pts |",
            pct(cj, tot),
            pct(ct, tot),
            pct(ct, tot) - pct(cj, tot)
        )?;
        writeln!(
            md,
            "\n> Parity Δ near zero ⇒ TOON preserves comprehension; large negative ⇒ compression hurts the model.\n"
        )?;
    }

    let md_path = out_dir.join("report.md");
    fs::write(&md_path, &md)?;

    // ---- Machine summary ----
    let summary = serde_json::json!({
        "items": pipeline.len(),
        "compress_rate_pct": 100.0 * overall.compressed as f64 / overall.n.max(1) as f64,
        "mean_byte_savings_pct": mean(&overall.byte_savings),
        "mean_token_savings_pct": mean(&overall.token_savings),
        "roundtrip_tested": rt.len(),
        "roundtrip_failures": rt_fail.len(),
        "classification_accuracy_pct": 100.0 * correct as f64 / graded.len().max(1) as f64,
        "comprehension_questions": comprehension.len(),
    });
    fs::write(
        out_dir.join("summary.json"),
        serde_json::to_string_pretty(&summary)?,
    )?;

    println!("\nReport written to {}", md_path.display());
    print!("{md}");
    Ok(())
}
