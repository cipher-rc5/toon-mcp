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

/// Confidence bucket for format detection results.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectionConfidence {
    /// Detection was backed by a full parser validation or an exact fallback.
    Certain,
    /// Detection was based on a format heuristic rather than a full parse.
    Heuristic,
}

impl DetectionConfidence {
    /// Return a stable lowercase string identifier for logging and display.
    pub fn as_str(self) -> &'static str {
        match self {
            DetectionConfidence::Certain => "certain",
            DetectionConfidence::Heuristic => "heuristic",
        }
    }
}

/// Metadata returned by format detection for clients that need ambiguity data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectionMetadata {
    /// Detected format selected by the normal detection precedence rules.
    pub format: InputFormat,
    /// Confidence bucket for the selected format.
    pub confidence: DetectionConfidence,
    /// Whether more than one detector matched the input.
    pub ambiguous: bool,
    /// All formats that matched, in detection precedence order.
    pub candidates: Vec<InputFormat>,
    /// Number of non-empty lines, populated when `format` is
    /// [`InputFormat::Jsonl`]. Counted during the detection probe so callers
    /// do not need a second scan over the input.
    pub line_count: Option<usize>,
}

impl DetectionMetadata {
    /// Metadata for an input whose format was not (or could not be) detected:
    /// `Unknown`, `Certain`, not ambiguous, no candidates.
    ///
    /// Used when the compression pipeline rejects an input before detection
    /// runs (e.g. it exceeds the byte-length limit) but a caller still expects
    /// a `DetectionMetadata` value.
    pub fn unknown() -> Self {
        Self {
            format: InputFormat::Unknown,
            confidence: DetectionConfidence::Certain,
            ambiguous: false,
            candidates: Vec::new(),
            line_count: None,
        }
    }
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
        Self::detect_with_metadata(input).format
    }

    /// Detect the format of `input` and include confidence/ambiguity metadata.
    pub fn detect_with_metadata(input: &str) -> DetectionMetadata {
        Self::detect_inner(input, false).0
    }

    /// Detect the format of `input` and, when the JSON probe validated the
    /// document, hand back the parsed `serde_json::Value` from that same
    /// pass.
    ///
    /// Callers that go on to parse a detected-JSON input (the compression
    /// pipeline) should use this instead of [`Self::detect_with_metadata`]
    /// followed by a fresh `serde_json::from_str`: it collapses the probe's
    /// validation walk and the parse into one traversal of the payload.
    pub fn detect_with_metadata_and_value(
        input: &str,
    ) -> (DetectionMetadata, Option<serde_json::Value>) {
        Self::detect_inner(input, true)
    }

    fn detect_inner(
        input: &str,
        want_value: bool,
    ) -> (DetectionMetadata, Option<serde_json::Value>) {
        let mut candidates = Vec::new();
        let mut json_value = None;

        // 1. JSON probe — fast byte-level pre-check before the O(N) parse.
        // Metadata-only callers validate with `IgnoredAny` (no `Value` tree
        // allocation); pipeline callers ask for the value so the validation
        // pass doubles as the parse.
        let first_nonws = input.bytes().find(|b| !b.is_ascii_whitespace());
        if matches!(first_nonws, Some(b'{') | Some(b'[')) {
            if want_value {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(input) {
                    json_value = Some(v);
                    candidates.push(InputFormat::Json);
                }
            } else if serde_json::from_str::<serde::de::IgnoredAny>(input).is_ok() {
                candidates.push(InputFormat::Json);
            }
        }

        // 2. JSONL probe — validate the first two non-empty lines, counting
        // the rest so the line count needs no second scan.
        let jsonl_line_count = Self::probe_jsonl(input);
        if jsonl_line_count.is_some() {
            candidates.push(InputFormat::Jsonl);
        }

        // 3. CSV probe — comma delimiter.
        if Self::probe_delimited(input, b',') {
            candidates.push(InputFormat::Csv);
        }

        // 4. TSV probe — tab delimiter.
        if Self::probe_delimited(input, b'\t') {
            candidates.push(InputFormat::Tsv);
        }

        let format = candidates.first().copied().unwrap_or(InputFormat::Unknown);
        let confidence = match format {
            InputFormat::Json | InputFormat::Unknown => DetectionConfidence::Certain,
            InputFormat::Jsonl | InputFormat::Csv | InputFormat::Tsv => {
                DetectionConfidence::Heuristic
            }
        };
        let metadata = DetectionMetadata {
            line_count: (format == InputFormat::Jsonl)
                .then_some(jsonl_line_count)
                .flatten(),
            format,
            confidence,
            ambiguous: candidates.len() > 1,
            candidates,
        };
        (metadata, json_value)
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

    /// Probe for JSONL: the first two non-empty lines must each be valid
    /// JSON. Returns the total non-empty line count on success so callers
    /// get the count from the same scan; `None` when the input is not JSONL.
    fn probe_jsonl(input: &str) -> Option<usize> {
        let mut checked = 0usize;
        let mut count = 0usize;
        for line in input.lines().filter(|l| !l.trim().is_empty()) {
            if checked < 2 {
                // `IgnoredAny` validates without allocating a `Value` tree.
                if serde_json::from_str::<serde::de::IgnoredAny>(line).is_err() {
                    return None;
                }
                checked += 1;
            }
            count += 1;
        }
        (checked == 2).then_some(count)
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
    fn detect_metadata_marks_json_certain() {
        let meta = FormatDetector::detect_with_metadata(r#"{"key":"value"}"#);
        assert_eq!(meta.format, InputFormat::Json);
        assert_eq!(meta.confidence, DetectionConfidence::Certain);
        assert!(!meta.ambiguous);
        assert_eq!(meta.candidates, vec![InputFormat::Json]);
    }

    #[test]
    fn detect_metadata_marks_csv_heuristic() {
        let meta = FormatDetector::detect_with_metadata("id,name\n1,Alice");
        assert_eq!(meta.format, InputFormat::Csv);
        assert_eq!(meta.confidence, DetectionConfidence::Heuristic);
        assert!(!meta.ambiguous);
        assert_eq!(meta.candidates, vec![InputFormat::Csv]);
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

    // --- L5: detect_and_parse round-trip tests for JSONL, CSV, TSV ---

    #[test]
    fn detect_and_parse_jsonl_returns_array() {
        let input = "{\"id\":1,\"name\":\"Alice\"}\n{\"id\":2,\"name\":\"Bob\"}";
        let (fmt, val) = FormatDetector::detect_and_parse(input).unwrap();
        assert_eq!(fmt, InputFormat::Jsonl);
        let arr = val.as_array().expect("value must be an array");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["id"], 1);
        assert_eq!(arr[1]["name"], "Bob");
    }

    #[test]
    fn detect_and_parse_csv_returns_array_of_objects() {
        let input = "id,name,score\n1,Alice,9.5\n2,Bob,8.0";
        let (fmt, val) = FormatDetector::detect_and_parse(input).unwrap();
        assert_eq!(fmt, InputFormat::Csv);
        let arr = val.as_array().expect("value must be an array");
        assert_eq!(arr.len(), 2);
        // Numeric coercion: id should be a number.
        assert!(
            arr[0]["id"].is_number(),
            "id should be numeric after coercion"
        );
        assert_eq!(arr[0]["name"], "Alice");
    }

    #[test]
    fn detect_and_parse_tsv_returns_array_of_objects() {
        let input = "id\tname\tscore\n1\tAlice\t9.5\n2\tBob\t8.0";
        let (fmt, val) = FormatDetector::detect_and_parse(input).unwrap();
        assert_eq!(fmt, InputFormat::Tsv);
        let arr = val.as_array().expect("value must be an array");
        assert_eq!(arr.len(), 2);
        assert!(
            arr[0]["id"].is_number(),
            "id should be numeric after coercion"
        );
        assert_eq!(arr[1]["name"], "Bob");
    }

    // --- L6: edge-case tests ---

    #[test]
    fn detect_empty_string_is_unknown() {
        assert_eq!(FormatDetector::detect(""), InputFormat::Unknown);
    }

    #[test]
    fn detect_whitespace_only_is_unknown() {
        assert_eq!(FormatDetector::detect("   \t\n  "), InputFormat::Unknown);
    }

    #[test]
    fn detect_single_jsonl_line_is_not_jsonl() {
        // JSONL requires at least two non-empty lines.
        let input = r#"{"id":1}"#;
        // A single JSON object is detected as JSON, not JSONL.
        assert_eq!(FormatDetector::detect(input), InputFormat::Json);
    }

    #[test]
    fn detect_csv_header_only_is_not_csv() {
        // A single-row CSV has no data rows — column count probe fails.
        let input = "id,name,score";
        // With no data row the probe returns false (only headers, no record).
        // It should fall through to Unknown.
        let fmt = FormatDetector::detect(input);
        assert_ne!(fmt, InputFormat::Csv, "single header row should not be Csv");
    }

    #[test]
    fn detect_unicode_json() {
        let input = r#"{"name":"日本語","value":42}"#;
        assert_eq!(FormatDetector::detect(input), InputFormat::Json);
    }

    #[test]
    fn detect_unicode_jsonl() {
        let input = "{\"city\":\"Tōkyō\"}\n{\"city\":\"Ōsaka\"}";
        assert_eq!(FormatDetector::detect(input), InputFormat::Jsonl);
    }

    #[test]
    fn detect_csv_with_unicode_fields() {
        let input = "name,city\nAlice,東京\nBob,大阪";
        assert_eq!(FormatDetector::detect(input), InputFormat::Csv);
    }
}
