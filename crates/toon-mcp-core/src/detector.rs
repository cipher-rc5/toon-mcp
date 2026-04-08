// file: crates/toon-mcp-core/src/detector.rs
// description: Format detection pipeline and InputFormat enum

use crate::{
    error::CoreError,
    parser::{Parser, csv::CsvParser, json::JsonParser, jsonl::JsonlParser},
};

/// The set of structured input formats that the detection pipeline recognises.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputFormat {
    /// Valid JSON — object or array root.
    Json,
    /// Newline-delimited JSON with two or more lines.
    Jsonl,
    /// Comma-delimited tabular text with two or more columns.
    Csv,
    /// Tab-delimited tabular text with two or more columns.
    Tsv,
    /// None of the above; content is passed through unchanged.
    Unknown,
}

impl InputFormat {
    /// Return a stable lowercase string identifier for logging and display.
    ///
    /// # Examples
    ///
    /// ```
    /// use toon_mcp_core::InputFormat;
    ///
    /// assert_eq!(InputFormat::Json.as_str(), "json");
    /// assert_eq!(InputFormat::Jsonl.as_str(), "jsonl");
    /// assert_eq!(InputFormat::Csv.as_str(), "csv");
    /// assert_eq!(InputFormat::Tsv.as_str(), "tsv");
    /// assert_eq!(InputFormat::Unknown.as_str(), "unknown");
    /// ```
    pub fn as_str(self) -> &'static str {
        match self {
            InputFormat::Json => "json",
            InputFormat::Jsonl => "jsonl",
            InputFormat::Csv => "csv",
            InputFormat::Tsv => "tsv",
            InputFormat::Unknown => "unknown",
        }
    }
}

impl std::fmt::Display for InputFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Stateless format detector and parser dispatcher.
///
/// Detection is ordered by specificity and cost:
/// 1. JSON probe (full `serde_json::from_str`)
/// 2. JSONL probe (parse first two non-empty lines)
/// 3. CSV probe (column count on first two rows)
/// 4. TSV probe (same, tab delimiter)
/// 5. Fallthrough to `InputFormat::Unknown`
pub struct FormatDetector;

impl FormatDetector {
    /// Return the number of columns in the first header row for CSV/TSV input,
    /// or `None` for other formats.
    ///
    /// This avoids re-importing the `csv` crate in the server layer.
    ///
    /// # Examples
    ///
    /// ```
    /// use toon_mcp_core::{FormatDetector, InputFormat};
    ///
    /// assert_eq!(
    ///     FormatDetector::column_count(InputFormat::Csv, "id,name,score\n1,Alice,9.5"),
    ///     Some(3)
    /// );
    /// assert_eq!(FormatDetector::column_count(InputFormat::Json, r#"{"a":1}"#), None);
    /// ```
    pub fn column_count(fmt: InputFormat, input: &str) -> Option<usize> {
        let delim = match fmt {
            InputFormat::Csv => b',',
            InputFormat::Tsv => b'\t',
            _ => return None,
        };
        csv::ReaderBuilder::new()
            .delimiter(delim)
            .from_reader(input.as_bytes())
            .headers()
            .ok()
            .map(|h| h.len())
    }

    /// Count non-empty lines in JSONL input, or return `None` for other formats.
    ///
    /// # Examples
    ///
    /// ```
    /// use toon_mcp_core::{FormatDetector, InputFormat};
    ///
    /// assert_eq!(
    ///     FormatDetector::jsonl_line_count(InputFormat::Jsonl, "{\"a\":1}\n\n{\"b\":2}"),
    ///     Some(2)
    /// );
    /// assert_eq!(FormatDetector::jsonl_line_count(InputFormat::Json, r#"{"a":1}"#), None);
    /// ```
    pub fn jsonl_line_count(fmt: InputFormat, input: &str) -> Option<usize> {
        if fmt == InputFormat::Jsonl {
            Some(input.lines().filter(|l| !l.trim().is_empty()).count())
        } else {
            None
        }
    }

    /// Detect the format of `input` without parsing the full document.
    ///
    /// Detection operates solely on the string slice; no I/O is performed.
    ///
    /// # Examples
    ///
    /// ```
    /// use toon_mcp_core::{FormatDetector, InputFormat};
    ///
    /// assert_eq!(FormatDetector::detect(r#"{"key":"value"}"#), InputFormat::Json);
    /// assert_eq!(FormatDetector::detect("{\"a\":1}\n{\"b\":2}"), InputFormat::Jsonl);
    /// assert_eq!(FormatDetector::detect("id,name\n1,Alice"), InputFormat::Csv);
    /// assert_eq!(FormatDetector::detect("plain text"), InputFormat::Unknown);
    /// ```
    pub fn detect(input: &str) -> InputFormat {
        // 1. JSON probe — O(N) full-document parse; runs first so that
        //    single-line JSON objects do not fall through to the JSONL probe.
        if serde_json::from_str::<serde_json::Value>(input).is_ok() {
            return InputFormat::Json;
        }

        // 2. JSONL probe — try first two non-empty lines.
        if Self::probe_jsonl(input) {
            return InputFormat::Jsonl;
        }

        // 3. CSV probe — comma delimiter.
        if Self::probe_delimited(input, b',') {
            return InputFormat::Csv;
        }

        // 4. TSV probe — tab delimiter.
        if Self::probe_delimited(input, b'\t') {
            return InputFormat::Tsv;
        }

        InputFormat::Unknown
    }

    /// Detect the format and immediately parse to a normalised `serde_json::Value`.
    ///
    /// Returns the detected format alongside the parsed value. The classifier
    /// and compressor operate exclusively on the returned value tree.
    ///
    /// # Examples
    ///
    /// ```
    /// use toon_mcp_core::{FormatDetector, InputFormat};
    ///
    /// let (fmt, val) = FormatDetector::detect_and_parse(r#"{"x":1}"#).unwrap();
    /// assert_eq!(fmt, InputFormat::Json);
    /// assert!(val.is_object());
    ///
    /// // Unknown format returns an error.
    /// assert!(FormatDetector::detect_and_parse("plain text").is_err());
    /// ```
    pub fn detect_and_parse(input: &str) -> Result<(InputFormat, serde_json::Value), CoreError> {
        let fmt = Self::detect(input);
        let value = match fmt {
            InputFormat::Json => JsonParser.parse(input)?,
            InputFormat::Jsonl => JsonlParser.parse(input)?,
            InputFormat::Csv => CsvParser::csv().parse(input)?,
            InputFormat::Tsv => CsvParser::tsv().parse(input)?,
            InputFormat::Unknown => {
                return Err(CoreError::ParseFailed {
                    format: InputFormat::Unknown,
                    line: 0,
                    detail: "unknown format — no parser available".into(),
                });
            }
        };
        Ok((fmt, value))
    }

    // --- private helpers ---

    fn probe_jsonl(input: &str) -> bool {
        let lines: Vec<&str> = input
            .lines()
            .filter(|l| !l.trim().is_empty())
            .take(2)
            .collect();

        if lines.len() < 2 {
            return false;
        }

        lines
            .iter()
            .all(|l| serde_json::from_str::<serde_json::Value>(l).is_ok())
    }

    fn probe_delimited(input: &str, delimiter: u8) -> bool {
        let mut rdr = csv::ReaderBuilder::new()
            .delimiter(delimiter)
            .has_headers(true)
            .from_reader(input.as_bytes());

        let headers = match rdr.headers() {
            Ok(h) => h.len(),
            Err(_) => return false,
        };

        if headers < 2 {
            return false;
        }

        // Check that first data record has the same column count.
        match rdr.records().next() {
            Some(Ok(record)) => record.len() == headers,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_json_object() {
        assert_eq!(
            FormatDetector::detect(r#"{"key":"value"}"#),
            InputFormat::Json
        );
    }

    #[test]
    fn detect_json_array() {
        assert_eq!(FormatDetector::detect(r#"[1,2,3]"#), InputFormat::Json);
    }

    #[test]
    fn detect_jsonl() {
        let input = "{\"id\":1}\n{\"id\":2}\n{\"id\":3}";
        assert_eq!(FormatDetector::detect(input), InputFormat::Jsonl);
    }

    #[test]
    fn detect_csv() {
        let input = "id,name,score\n1,Alice,9.5\n2,Bob,8.0";
        assert_eq!(FormatDetector::detect(input), InputFormat::Csv);
    }

    #[test]
    fn detect_tsv() {
        let input = "id\tname\tscore\n1\tAlice\t9.5\n2\tBob\t8.0";
        assert_eq!(FormatDetector::detect(input), InputFormat::Tsv);
    }

    #[test]
    fn detect_unknown() {
        assert_eq!(
            FormatDetector::detect("this is plain text"),
            InputFormat::Unknown
        );
    }

    /// JSON wins over JSONL on a single-line valid JSON string.
    #[test]
    fn json_wins_over_jsonl_on_single_line() {
        // A single JSON object on one line could also look like JSONL with
        // one record, but JSON probe runs first.
        let input = r#"{"id":1}"#;
        assert_eq!(FormatDetector::detect(input), InputFormat::Json);
    }

    #[test]
    fn detect_and_parse_json() {
        let (fmt, val) = FormatDetector::detect_and_parse(r#"{"x":1}"#).unwrap();
        assert_eq!(fmt, InputFormat::Json);
        assert!(val.is_object());
    }

    #[test]
    fn detect_and_parse_unknown_returns_error() {
        let err = FormatDetector::detect_and_parse("plain text").unwrap_err();
        match err {
            CoreError::ParseFailed { format, .. } => {
                assert_eq!(format, InputFormat::Unknown);
            }
            other => panic!("unexpected error: {other}"),
        }
    }
}
