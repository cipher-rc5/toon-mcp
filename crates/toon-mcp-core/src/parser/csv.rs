// file: crates/toon-mcp-core/src/parser/csv.rs
// description: CSV/TSV parser — normalises tabular input to a Value::Array of objects
// reference: https://docs.rs/csv/latest/csv/

use crate::{error::CoreError, parser::Parser};
use serde_json::{Map, Number, Value};

/// Metadata describing CSV/TSV numeric coercion for a specific parse request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CsvCoercionMetadata {
    /// Whether numeric-looking fields were eligible for coercion.
    pub numeric_coercion_used: bool,
    /// Whether at least one numeric-looking field has syntax that may lose
    /// caller intent when represented as a JSON number, such as leading
    /// zeroes or integer literals longer than 15 digits.
    pub lossy_coercion_possible: bool,
}

/// Parses CSV or TSV input into a `Value::Array` of uniform `Value::Object` rows.
///
/// The first record is treated as a header row. Each subsequent record becomes
/// an object whose keys are the header names and whose values are either
/// `Value::Number` or `Value::String`. Coercion is two-stage: fields that
/// parse as `i64` become integer numbers; fields containing `.`, `e`, or `E`
/// fall back to `f64`. Integer literals whose magnitude exceeds 2^53 stay
/// `Value::String` so the exact digits survive downstream f64 consumers.
///
/// Numeric coercion improves TOON savings because numbers are emitted without
/// quotes in the encoded output.
///
/// # Examples
///
/// ```
/// use toon_mcp_core::parser::{Parser, csv::CsvParser};
///
/// let val = CsvParser::csv().parse("id,name\n1,Alice\n2,Bob").unwrap();
/// let arr = val.as_array().unwrap();
/// assert_eq!(arr.len(), 2);
/// assert_eq!(arr[0]["name"], "Alice");
/// // Numeric fields are coerced.
/// assert!(arr[0]["id"].is_number());
///
/// // TSV variant uses a tab delimiter.
/// let val = CsvParser::tsv().parse("x\ty\n1\t2").unwrap();
/// assert!(val[0]["x"].is_number());
/// ```
pub struct CsvParser {
    /// The field delimiter byte (`b','` for CSV, `b'\t'` for TSV).
    delimiter: u8,
    /// Whether to coerce numeric-looking fields into `Value::Number`.
    ///
    /// When `false`, every field is emitted as `Value::String` regardless of
    /// content. Disable this when inputs contain identifiers, postal codes,
    /// or leading-zero values that must not be silently coerced.
    numeric_coercion: bool,
}

impl CsvParser {
    /// Create a new parser with the given field delimiter.
    ///
    /// Numeric coercion is enabled by default; use
    /// [`Self::with_numeric_coercion`] to disable it.
    pub fn new(delimiter: u8) -> Self {
        Self {
            delimiter,
            numeric_coercion: true,
        }
    }

    /// Create a CSV parser (comma delimiter).
    pub fn csv() -> Self {
        Self::new(b',')
    }

    /// Create a TSV parser (tab delimiter).
    pub fn tsv() -> Self {
        Self::new(b'\t')
    }

    /// Toggle numeric coercion for parsed fields.
    ///
    /// When `enabled` is `true` (the default), integer-looking fields become
    /// `i64` numbers and decimal/exponent fields become `f64` numbers. When
    /// `false`, all fields remain as `Value::String`, which preserves
    /// identifiers, postal codes, and leading-zero values verbatim at the
    /// cost of larger TOON output.
    pub fn with_numeric_coercion(mut self, enabled: bool) -> Self {
        self.numeric_coercion = enabled;
        self
    }

    /// Inspect input for numeric-coercion visibility without producing values.
    ///
    /// `numeric_coercion_used` is true only when coercion is enabled and at
    /// least one data field was coerced into a JSON number.
    /// `lossy_coercion_possible` flags number-like fields whose textual form
    /// carries information JSON numbers do not preserve exactly, including
    /// leading zeroes, explicit plus signs, exponent notation, decimal
    /// spellings of whole numbers, and integer literals longer than 15 digits
    /// (which exceed exact f64 representability in downstream consumers).
    pub fn coercion_metadata(&self, input: &str) -> Result<CsvCoercionMetadata, CoreError> {
        let input = crate::parser::strip_bom(input);
        let mut rdr = csv::ReaderBuilder::new()
            .delimiter(self.delimiter)
            .from_reader(input.as_bytes());

        let _headers = rdr.headers()?;
        let mut numeric_coercion_used = false;
        let mut lossy_coercion_possible = false;

        if !self.numeric_coercion {
            return Ok(CsvCoercionMetadata {
                numeric_coercion_used,
                lossy_coercion_possible,
            });
        }

        for result in rdr.records() {
            let record = result?;
            for field in &record {
                match coerce_numeric_field(field) {
                    FieldCoercion::Number(_) => {
                        numeric_coercion_used = true;
                        lossy_coercion_possible |= numeric_text_may_be_lossy(field);
                    }
                    FieldCoercion::PreservedInteger => {
                        lossy_coercion_possible = true;
                    }
                    FieldCoercion::Text => {}
                }
            }
        }

        Ok(CsvCoercionMetadata {
            numeric_coercion_used,
            lossy_coercion_possible,
        })
    }
}

/// Largest integer magnitude exactly representable as an IEEE 754 f64 (2^53).
///
/// Integer literals beyond this bound stay `Value::String`: a downstream
/// consumer that reads JSON numbers as f64 (JavaScript, many analytics
/// stacks) would silently round them, so the exact digits must survive.
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_992;

/// Outcome of the two-stage numeric coercion attempt on a single field.
enum FieldCoercion {
    /// The field coerces to a JSON number (i64 first, f64 fallback).
    Number(Number),
    /// The field is an integer literal too large to coerce without precision
    /// risk; it must be preserved verbatim as `Value::String`.
    PreservedInteger,
    /// The field is not numeric and remains `Value::String`.
    Text,
}

/// Attempt numeric coercion: `i64` first, then `f64` only when the trimmed
/// field contains `.`, `e`, or `E`. Integer literals whose magnitude exceeds
/// [`MAX_SAFE_INTEGER`] are never coerced.
fn coerce_numeric_field(field: &str) -> FieldCoercion {
    if let Ok(i) = field.parse::<i64>() {
        return if i.unsigned_abs() > MAX_SAFE_INTEGER {
            FieldCoercion::PreservedInteger
        } else {
            FieldCoercion::Number(Number::from(i))
        };
    }

    let trimmed = field.trim();
    if trimmed.contains(['.', 'e', 'E']) {
        if let Ok(n) = field.parse::<f64>()
            && let Some(num) = Number::from_f64(n)
        {
            // Finite f64: coerce to a JSON number. Infinite/NaN fields fail
            // `from_f64` and fall through to string.
            return FieldCoercion::Number(num);
        }
        return FieldCoercion::Text;
    }

    // An integer literal that overflowed i64 is by definition far beyond
    // MAX_SAFE_INTEGER; preserve it. `field` (not `trimmed`) keeps
    // whitespace-padded fields on the plain-text path, matching the parse
    // attempts above which also reject surrounding whitespace.
    if is_integer_literal(field) {
        return FieldCoercion::PreservedInteger;
    }

    FieldCoercion::Text
}

/// Whether `field` is an optionally signed run of ASCII digits.
fn is_integer_literal(field: &str) -> bool {
    let unsigned = field.strip_prefix(['-', '+']).unwrap_or(field);
    !unsigned.is_empty() && unsigned.bytes().all(|b| b.is_ascii_digit())
}

fn numeric_text_may_be_lossy(field: &str) -> bool {
    let trimmed = field.trim();
    if trimmed.starts_with('+') || trimmed.contains(['e', 'E']) {
        return true;
    }

    let unsigned = trimmed.strip_prefix('-').unwrap_or(trimmed);
    let integer_part = unsigned.split_once('.').map_or(unsigned, |(left, _)| left);
    if integer_part.len() > 1 && integer_part.starts_with('0') {
        return true;
    }

    // Integer literals longer than 15 digits can exceed the contiguous range
    // of exactly representable f64 integers; flag them even when this parser
    // kept exact i64 precision, because downstream consumers may not.
    if !unsigned.contains('.') && integer_part.len() > 15 {
        return true;
    }

    trimmed.contains('.')
        && field
            .parse::<f64>()
            .is_ok_and(|parsed| parsed.fract() == 0.0)
}

impl CsvParser {
    /// Parse `input` while simultaneously collecting numeric-coercion metadata.
    ///
    /// Equivalent to calling [`Parser::parse`] and [`Self::coercion_metadata`]
    /// back-to-back, but walks the input only once. The compression pipeline
    /// needs both the parsed value and the coercion visibility flags, so the
    /// single pass avoids re-reading and re-parsing the entire CSV body.
    pub fn parse_with_coercion_metadata(
        &self,
        input: &str,
    ) -> Result<(Value, CsvCoercionMetadata), CoreError> {
        let input = crate::parser::strip_bom(input);
        let mut rdr = csv::ReaderBuilder::new()
            .delimiter(self.delimiter)
            .from_reader(input.as_bytes());

        // Headers must be owned `String`s: the `StringRecord` returned by
        // `rdr.headers()` borrows `rdr`, but the row loop below needs its own
        // mutable borrow of `rdr` via `records()`. Owning the header names
        // decouples them from the reader. The per-row loop then zips
        // `&str` slices, avoiding a second `Vec<&str>` collection.
        let headers: Vec<String> = rdr.headers()?.iter().map(str::to_owned).collect();

        // Reject duplicate headers up-front: `serde_json::Map` would otherwise
        // collapse duplicate columns into a single key, silently dropping data.
        // O(headers) — runs once before the row loop, not per-row.
        let mut seen = std::collections::HashSet::with_capacity(headers.len());
        for h in &headers {
            if !seen.insert(h.as_str()) {
                return Err(CoreError::DuplicateHeader { header: h.clone() });
            }
        }

        let mut rows: Vec<Value> = Vec::new();
        let mut numeric_coercion_used = false;
        let mut lossy_coercion_possible = false;

        for result in rdr.records() {
            let record = result?;
            let mut map = Map::with_capacity(headers.len());

            for (key, field) in headers.iter().zip(record.iter()) {
                let val = if self.numeric_coercion {
                    match coerce_numeric_field(field) {
                        FieldCoercion::Number(num) => {
                            numeric_coercion_used = true;
                            lossy_coercion_possible |= numeric_text_may_be_lossy(field);
                            Value::Number(num)
                        }
                        FieldCoercion::PreservedInteger => {
                            // Too large to represent without precision risk:
                            // keep the exact digits and flag the near-miss.
                            lossy_coercion_possible = true;
                            Value::String(field.to_owned())
                        }
                        FieldCoercion::Text => Value::String(field.to_owned()),
                    }
                } else {
                    Value::String(field.to_owned())
                };
                // `to_owned()` allocates the single owned key that
                // `serde_json::Map<String, Value>` requires.
                map.insert(key.to_owned(), val);
            }

            rows.push(Value::Object(map));
        }

        Ok((
            Value::Array(rows),
            CsvCoercionMetadata {
                numeric_coercion_used,
                lossy_coercion_possible,
            },
        ))
    }
}

impl Parser for CsvParser {
    fn parse(&self, input: &str) -> Result<Value, CoreError> {
        self.parse_with_coercion_metadata(input).map(|(v, _)| v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_csv_with_header() {
        let input = "id,name,score\n1,Alice,9.5\n2,Bob,8.0";
        let p = CsvParser::csv();
        let v = p.parse(input).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["id"], 1.0);
        assert_eq!(arr[0]["name"], "Alice");
        assert_eq!(arr[0]["score"], 9.5);
    }

    #[test]
    fn parse_tsv_delimiter() {
        let input = "id\tname\n1\tAlice\n2\tBob";
        let p = CsvParser::tsv();
        let v = p.parse(input).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["name"], "Alice");
    }

    #[test]
    fn numeric_coercion() {
        let input = "x,y\n1,2\n3,4";
        let p = CsvParser::csv();
        let v = p.parse(input).unwrap();
        let arr = v.as_array().unwrap();
        // Values should be numbers, not strings
        assert!(arr[0]["x"].is_number());
        assert!(arr[0]["y"].is_number());
    }

    #[test]
    fn non_numeric_stays_string() {
        let input = "tag,value\nhello,world";
        let p = CsvParser::csv();
        let v = p.parse(input).unwrap();
        assert_eq!(v[0]["tag"], "hello");
        assert_eq!(v[0]["value"], "world");
    }

    // --- L6: edge-case tests ---

    #[test]
    fn header_only_csv_returns_empty_array() {
        // A CSV with only a header row and no data rows should parse to an
        // empty array (no records).
        let input = "id,name,score";
        let p = CsvParser::csv();
        let v = p.parse(input).unwrap();
        assert!(v.as_array().unwrap().is_empty());
    }

    #[test]
    fn csv_with_unicode_values_parses() {
        let input = "name,city\nAlice,東京\nBob,大阪";
        let p = CsvParser::csv();
        let v = p.parse(input).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["city"], "東京");
        assert_eq!(arr[1]["name"], "Bob");
    }

    #[test]
    fn disabled_coercion_preserves_strings() {
        // With numeric_coercion disabled, all fields stay as Value::String —
        // useful for inputs containing identifiers, postal codes, or
        // leading-zero values that must not be silently coerced to f64.
        let v = CsvParser::csv()
            .with_numeric_coercion(false)
            .parse("id,x\n1,2")
            .unwrap();
        assert!(v[0]["id"].is_string());
        assert!(v[0]["x"].is_string());
    }

    #[test]
    fn coercion_metadata_reports_numeric_fields() {
        let meta = CsvParser::csv()
            .coercion_metadata("id,name\n1,Alice")
            .unwrap();
        assert!(meta.numeric_coercion_used);
        assert!(!meta.lossy_coercion_possible);
    }

    #[test]
    fn coercion_metadata_flags_lossy_spellings() {
        let meta = CsvParser::csv()
            .coercion_metadata("zip,count\n00123,1.0")
            .unwrap();
        assert!(meta.numeric_coercion_used);
        assert!(meta.lossy_coercion_possible);
    }

    #[test]
    fn disabled_coercion_metadata_reports_no_coercion() {
        let meta = CsvParser::csv()
            .with_numeric_coercion(false)
            .coercion_metadata("id\n1")
            .unwrap();
        assert!(!meta.numeric_coercion_used);
        assert!(!meta.lossy_coercion_possible);
    }

    #[test]
    fn duplicate_csv_headers_are_rejected() {
        // The `id` header appears twice; the parser must reject the input
        // rather than silently collapse both columns into a single map key.
        let input = "id,name,id\n1,Alice,2";
        let err = CsvParser::csv().parse(input).expect_err("must reject");
        match err {
            CoreError::DuplicateHeader { header } => assert_eq!(header, "id"),
            other => panic!("expected DuplicateHeader, got {other:?}"),
        }
    }

    #[test]
    fn duplicate_tsv_headers_are_rejected() {
        // Same shape as the CSV case but with a tab delimiter; the TSV parser
        // must also surface the duplicate-header error.
        let input = "id\tname\tid\n1\tAlice\t2";
        let err = CsvParser::tsv().parse(input).expect_err("must reject");
        match err {
            CoreError::DuplicateHeader { header } => assert_eq!(header, "id"),
            other => panic!("expected DuplicateHeader, got {other:?}"),
        }
    }

    #[test]
    fn case_sensitive_headers_are_distinct() {
        // Header names differ only by case; they are distinct keys in
        // `serde_json::Map` (case-sensitive) and must both survive parsing.
        let input = "id,ID\n1,2";
        let v = CsvParser::csv().parse(input).expect("parse");
        let obj = v[0].as_object().expect("row is object");
        assert!(obj.contains_key("id"));
        assert!(obj.contains_key("ID"));
    }

    #[test]
    fn csv_integer_column_stays_integer() {
        // Integer-looking fields must coerce through the i64 path, not f64:
        // an f64 detour would serialise `1` as `1.0` and corrupt identifiers.
        let input = "id,v\n1,a\n2,b\n3,c";
        let v = CsvParser::csv().parse(input).unwrap();
        let arr = v.as_array().unwrap();
        for (idx, expected) in [(0usize, 1i64), (1, 2), (2, 3)] {
            let num = &arr[idx]["id"];
            assert!(num.is_i64(), "id must be an i64 number, got {num:?}");
            assert!(!num.is_f64(), "id must not be an f64 number");
            assert_eq!(num.as_i64(), Some(expected));
        }
        assert_eq!(serde_json::to_string(&arr[0]["id"]).unwrap(), "1");
    }

    #[test]
    fn csv_large_integer_id_preserved_as_string() {
        // 1234567890123456789 fits i64 but exceeds 2^53, so f64 consumers
        // would round it. The exact text must survive as a string.
        let input = "id,v\n1234567890123456789,a";
        let v = CsvParser::csv().parse(input).unwrap();
        assert_eq!(v[0]["id"], "1234567890123456789");
    }

    #[test]
    fn csv_large_integer_flags_lossy() {
        let meta = CsvParser::csv()
            .coercion_metadata("id,v\n1234567890123456789,a")
            .unwrap();
        assert!(meta.lossy_coercion_possible);
    }

    #[test]
    fn csv_float_column_still_coerces() {
        let input = "id,score\n1,9.5";
        let v = CsvParser::csv().parse(input).unwrap();
        let score = &v[0]["score"];
        assert!(score.is_f64(), "score must remain an f64, got {score:?}");
        assert_eq!(score.as_f64(), Some(9.5));
    }

    #[test]
    fn integer_beyond_i64_preserved_as_string() {
        // 20 digits overflows i64 entirely; the literal must stay a string
        // and still raise the lossy flag for callers.
        let input = "id\n99999999999999999999";
        let p = CsvParser::csv();
        let v = p.parse(input).unwrap();
        assert_eq!(v[0]["id"], "99999999999999999999");
        let meta = p.coercion_metadata(input).unwrap();
        assert!(meta.lossy_coercion_possible);
    }

    #[test]
    fn leading_bom_is_stripped() {
        // Without stripping, the BOM would glue onto the first header name.
        let v = CsvParser::csv()
            .parse("\u{feff}id,name\n1,Alice")
            .expect("parse");
        let obj = v[0].as_object().expect("row is object");
        assert!(obj.contains_key("id"), "BOM must not corrupt the header");
        let meta = CsvParser::csv()
            .coercion_metadata("\u{feff}id,name\n1,Alice")
            .expect("metadata");
        assert!(meta.numeric_coercion_used);
    }

    #[test]
    fn infinity_and_nan_fields_become_strings() {
        // f64::INFINITY and f64::NAN cannot be represented as serde_json::Number,
        // so the parser should fall back to a string for those fields.
        let input = "x,y\ninf,nan\n1,2";
        let p = CsvParser::csv();
        let v = p.parse(input).unwrap();
        let arr = v.as_array().unwrap();
        // "inf" parses as f64::INFINITY which is non-finite, so becomes a String.
        assert!(arr[0]["x"].is_string(), "inf should become a string");
        // "nan" also parses as NaN which is non-finite.
        assert!(arr[0]["y"].is_string(), "nan should become a string");
        // Normal numeric fields still coerce.
        assert!(arr[1]["x"].is_number());
    }
}

#[cfg(test)]
mod proptest_tests {
    // file: crates/toon-mcp-core/src/parser/csv.rs (proptest_tests)
    // description: Round-trip proptests for CSV and TSV parsers using generated tables.

    use super::*;
    use proptest::prelude::*;

    /// A header name strategy: alpha-only (so it never coerces to a number),
    /// non-empty, no delimiter or newline characters.
    fn header_strategy() -> impl Strategy<Value = String> {
        "[a-zA-Z][a-zA-Z0-9_]{0,7}"
    }

    /// A cell value strategy: starts with a letter so the f64 parse always
    /// fails and the parser yields a Value::String. Avoids commas, tabs,
    /// quotes, newlines, and carriage returns to dodge CSV escaping.
    fn cell_strategy() -> impl Strategy<Value = String> {
        "[a-zA-Z][a-zA-Z0-9_ -]{0,7}"
    }

    /// Generate a (headers, rows) pair. Headers are unique within the table
    /// because `csv::Reader` would otherwise fold duplicate columns into a
    /// single map key, breaking the round trip property.
    fn table_strategy(
        cols: std::ops::Range<usize>,
        rows: std::ops::Range<usize>,
    ) -> impl Strategy<Value = (Vec<String>, Vec<Vec<String>>)> {
        prop::collection::vec(header_strategy(), cols)
            .prop_filter("headers must be unique", |hs| {
                let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
                hs.iter().all(|h| seen.insert(h.as_str()))
            })
            .prop_flat_map(move |headers| {
                let n = headers.len();
                let rows_strategy = prop::collection::vec(
                    prop::collection::vec(cell_strategy(), n..=n),
                    rows.clone(),
                );
                (Just(headers), rows_strategy)
            })
    }

    /// Render a table to a string using the given delimiter byte.
    fn render_table(headers: &[String], rows: &[Vec<String>], delim: char) -> String {
        let d = delim.to_string();
        let mut out = String::new();
        out.push_str(&headers.join(&d));
        for row in rows {
            out.push('\n');
            out.push_str(&row.join(&d));
        }
        out
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// Round-trip a CSV table: headers and string cells survive parsing
        /// into a `Value::Array` of `Value::Object` rows with matching keys
        /// and string values.
        #[test]
        fn csv_round_trip_preserves_string_cells(
            (headers, rows) in table_strategy(1..6, 1..6)
        ) {
            let input = render_table(&headers, &rows, ',');
            let parsed = CsvParser::csv().parse(&input).expect("parse");
            let arr = parsed.as_array().expect("array root");
            prop_assert_eq!(arr.len(), rows.len());
            for (row_idx, row_cells) in rows.iter().enumerate() {
                let obj = arr[row_idx].as_object().expect("row is object");
                prop_assert_eq!(obj.len(), headers.len());
                for (col_idx, header) in headers.iter().enumerate() {
                    let val = obj.get(header).expect("header present");
                    let s = val.as_str().expect("value is string");
                    prop_assert_eq!(s, row_cells[col_idx].as_str());
                }
            }
        }

        /// Integer-valued cells round-trip exactly: every generated i64 in
        /// the f64-safe range must come back as the same `is_i64` number,
        /// never an f64 approximation.
        #[test]
        fn csv_round_trip_preserves_integer_cells(
            headers in prop::collection::vec(header_strategy(), 1..4)
                .prop_filter("headers must be unique", |hs| {
                    let mut seen: std::collections::HashSet<&str> =
                        std::collections::HashSet::new();
                    hs.iter().all(|h| seen.insert(h.as_str()))
                }),
            values in prop::collection::vec(
                prop::collection::vec(
                    -9_007_199_254_740_992i64..=9_007_199_254_740_992i64,
                    1..4,
                ),
                1..6,
            ),
        ) {
            let cols = headers.len();
            let rows: Vec<Vec<String>> = values
                .iter()
                .map(|row| {
                    (0..cols)
                        .map(|c| row[c % row.len()].to_string())
                        .collect()
                })
                .collect();
            let input = render_table(&headers, &rows, ',');
            let parsed = CsvParser::csv().parse(&input).expect("parse");
            let arr = parsed.as_array().expect("array root");
            prop_assert_eq!(arr.len(), rows.len());
            for (row_idx, row_cells) in rows.iter().enumerate() {
                let obj = arr[row_idx].as_object().expect("row is object");
                for (col_idx, header) in headers.iter().enumerate() {
                    let val = obj.get(header).expect("header present");
                    prop_assert!(val.is_i64(), "expected i64, got {:?}", val);
                    let expected: i64 = row_cells[col_idx].parse().expect("i64 cell");
                    prop_assert_eq!(val.as_i64(), Some(expected));
                }
            }
        }

        /// Same property for TSV: header and string-only cells round-trip
        /// through a tab-delimited render and parse.
        #[test]
        fn tsv_round_trip_preserves_string_cells(
            (headers, rows) in table_strategy(1..6, 1..6)
        ) {
            let input = render_table(&headers, &rows, '\t');
            let parsed = CsvParser::tsv().parse(&input).expect("parse");
            let arr = parsed.as_array().expect("array root");
            prop_assert_eq!(arr.len(), rows.len());
            for (row_idx, row_cells) in rows.iter().enumerate() {
                let obj = arr[row_idx].as_object().expect("row is object");
                prop_assert_eq!(obj.len(), headers.len());
                for (col_idx, header) in headers.iter().enumerate() {
                    let val = obj.get(header).expect("header present");
                    let s = val.as_str().expect("value is string");
                    prop_assert_eq!(s, row_cells[col_idx].as_str());
                }
            }
        }
    }
}
