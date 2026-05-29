//! LLM comprehension-parity evaluation.
//!
//! For items that contain a record array we build questions whose answers are
//! computed deterministically from the parsed value (no LLM grading needed),
//! then ask gemma the SAME question twice — once with the original payload in
//! context, once with the TOON encoding. If TOON answer-accuracy tracks JSON
//! accuracy, the compression is "comprehension-safe".

use crate::corpus::CorpusItem;
use crate::llm::LlmClient;
use crate::toonkit::{encode_pipeline, parse_to_value};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComprehensionResult {
    pub id: String,
    pub format: String,
    pub intended_shape: String,
    pub qtype: String,
    pub question: String,
    pub ground_truth: String,
    pub answer_json: String,
    pub answer_toon: String,
    pub correct_json: bool,
    pub correct_toon: bool,
}

struct Question {
    qtype: &'static str,
    text: String,
    truth: String,
    numeric: bool,
}

/// Locate the first array of objects in the value tree (handles fold chains).
fn find_records(v: &Value) -> Option<&Vec<Value>> {
    match v {
        Value::Array(a)
            if a.len() >= 3 && a.iter().filter(|x| x.is_object()).count() * 2 >= a.len() =>
        {
            Some(a)
        }
        Value::Object(m) => m.values().find_map(find_records),
        Value::Array(a) => a.iter().find_map(find_records),
        _ => None,
    }
}

fn as_plain(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => v.to_string(),
    }
}

fn build_questions(records: &[Value]) -> Vec<Question> {
    let mut qs = Vec::new();

    // Q1 — count.
    qs.push(Question {
        qtype: "count",
        text: "How many records are in the dataset?".into(),
        truth: records.len().to_string(),
        numeric: true,
    });

    // Determine field sets from the first object.
    let Some(first) = records.iter().find_map(Value::as_object) else {
        return qs;
    };
    let mut keys: Vec<&String> = first.keys().collect();
    keys.sort();

    // A key field: prefer something id/name-like, else the first key whose
    // values are distinct across records.
    let key_field = keys
        .iter()
        .find(|k| {
            let lk = k.to_lowercase();
            lk.contains("id") || lk == "name" || lk.contains("name")
        })
        .copied()
        .or_else(|| keys.first().copied());

    // A non-key value field to look up.
    let value_field = keys
        .iter()
        .copied()
        .find(|k| Some(k.as_str()) != key_field.map(String::as_str));

    // Q2 — lookup: middle record, value of `value_field` where `key_field` == k.
    if let (Some(kf), Some(vf)) = (key_field, value_field) {
        let rec = &records[records.len() / 2];
        if let Value::Object(m) = rec
            && let (Some(kv), Some(vv)) = (m.get(kf), m.get(vf))
        {
            qs.push(Question {
                qtype: "lookup",
                text: format!(
                    "What is the value of `{vf}` for the record whose `{kf}` is {}?",
                    as_plain(kv)
                ),
                truth: as_plain(vv),
                numeric: vv.is_number(),
            });
        }
    }

    // Q3 — max of a numeric field across all records.
    let numeric_field = keys.iter().find(|k| {
        records
            .iter()
            .filter_map(Value::as_object)
            .filter_map(|m| m.get(**k))
            .any(Value::is_number)
    });
    if let Some(nf) = numeric_field {
        let max = records
            .iter()
            .filter_map(Value::as_object)
            .filter_map(|m| m.get(*nf))
            .filter_map(Value::as_f64)
            .fold(f64::NEG_INFINITY, f64::max);
        if max.is_finite() {
            qs.push(Question {
                qtype: "max",
                text: format!("What is the maximum value of `{nf}` across all records?"),
                truth: trim_num(max),
                numeric: true,
            });
        }
    }

    qs
}

fn trim_num(f: f64) -> String {
    if (f.fract()).abs() < 1e-9 {
        format!("{}", f as i64)
    } else {
        format!("{f}")
    }
}

fn first_number(s: &str) -> Option<f64> {
    let mut buf = String::new();
    let mut out = None;
    for ch in s.chars() {
        if ch.is_ascii_digit() || ch == '.' || ch == '-' {
            buf.push(ch);
        } else if !buf.is_empty() {
            if let Ok(n) = buf.parse::<f64>() {
                out = Some(n);
                break;
            }
            buf.clear();
        }
    }
    out.or_else(|| buf.parse::<f64>().ok())
}

fn normalize(s: &str) -> String {
    s.trim()
        .trim_matches(['"', '\'', '`', '.', ',', ' '])
        .to_lowercase()
}

fn grade(q: &Question, answer: &str) -> bool {
    if q.numeric {
        match (first_number(answer), q.truth.parse::<f64>().ok()) {
            (Some(a), Some(t)) => (a - t).abs() <= 1e-6 * t.abs().max(1.0),
            _ => false,
        }
    } else {
        let a = normalize(answer);
        let t = normalize(&q.truth);
        !t.is_empty() && (a == t || a.contains(&t))
    }
}

const ANSWER_SYS: &str = "You are a precise data-reading assistant. Answer the question using ONLY the dataset provided. Reply with just the answer value — no explanation, no units, no extra words.";

fn ask(client: &LlmClient, repr_label: &str, repr: &str, question: &str) -> Result<String> {
    let user = format!("Dataset ({repr_label}):\n{repr}\n\nQuestion: {question}");
    client.chat(ANSWER_SYS, &user, 0.0, 64)
}

pub struct ComprehendConfig {
    /// Skip items whose JSON payload exceeds this many bytes (context budget).
    pub max_context_bytes: usize,
}

impl Default for ComprehendConfig {
    fn default() -> Self {
        Self {
            max_context_bytes: 5000,
        }
    }
}

/// Run comprehension parity for one item; returns the per-question results.
pub fn evaluate_item(
    client: &LlmClient,
    item: &CorpusItem,
    cfg: &ComprehendConfig,
) -> Result<Vec<ComprehensionResult>> {
    if item.payload.len() > cfg.max_context_bytes {
        return Ok(vec![]);
    }
    let value = match parse_to_value(&item.format, &item.payload, true) {
        Ok(v) => v,
        Err(_) => return Ok(vec![]),
    };
    let Some(records) = find_records(&value) else {
        return Ok(vec![]);
    };
    let toon = match encode_pipeline(&value) {
        Ok(t) => t,
        Err(_) => return Ok(vec![]),
    };

    let mut out = Vec::new();
    for q in build_questions(records) {
        let answer_json = ask(client, &item.format.to_uppercase(), &item.payload, &q.text)?;
        let answer_toon = ask(client, "TOON", &toon, &q.text)?;
        out.push(ComprehensionResult {
            id: item.id.clone(),
            format: item.format.clone(),
            intended_shape: item.intended_shape.clone(),
            qtype: q.qtype.to_string(),
            question: q.text.clone(),
            correct_json: grade(&q, &answer_json),
            correct_toon: grade(&q, &answer_toon),
            ground_truth: q.truth,
            answer_json,
            answer_toon,
        });
    }
    Ok(out)
}
