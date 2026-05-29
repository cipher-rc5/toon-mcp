//! Corpus item model and JSONL persistence shared across eval stages.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

/// One generated payload plus the metadata the generator intended for it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusItem {
    pub id: String,
    /// `json` | `jsonl` | `csv` | `tsv`
    pub format: String,
    /// Free-text domain label, e.g. `ecommerce_orders`.
    pub category: String,
    /// Shape the generation spec was designed to elicit:
    /// `tabular` | `fold_chain` | `primitive_array` | `mixed` | `pass_through`.
    pub intended_shape: String,
    pub payload: String,
    pub bytes: usize,
}

pub fn read_jsonl<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Vec<T>> {
    let f = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut out = Vec::new();
    for (i, line) in BufReader::new(f).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let item: T = serde_json::from_str(&line)
            .with_context(|| format!("{}:{} invalid JSONL row", path.display(), i + 1))?;
        out.push(item);
    }
    Ok(out)
}

/// Truncate-and-write a JSONL file from scratch.
pub fn write_jsonl<T: Serialize>(path: &Path, items: &[T]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut f = File::create(path).with_context(|| format!("create {}", path.display()))?;
    for item in items {
        writeln!(f, "{}", serde_json::to_string(item)?)?;
    }
    Ok(())
}

/// Append a single row (used during generation so progress survives a crash).
pub struct JsonlAppender {
    file: File,
}

impl JsonlAppender {
    pub fn create(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)
            .with_context(|| format!("create {}", path.display()))?;
        Ok(Self { file })
    }

    pub fn push<T: Serialize>(&mut self, item: &T) -> Result<()> {
        writeln!(self.file, "{}", serde_json::to_string(item)?)?;
        self.file.flush()?;
        Ok(())
    }
}
