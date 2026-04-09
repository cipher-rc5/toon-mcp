// file: crates/toon-mcp-core/src/parser/csv.rs
// description: CSV/TSV parser — normalises tabular input to a Value::Array of objects
// reference: https://docs.rs/csv/latest/csv/

use crate::{error::CoreError, parser::Parser};
use serde_json::{Map, Number, Value};

/// Parses CSV or TSV input into a `Value::Array` of uniform `Value::Object` rows.
///
/// The first record is treated as a header row. Each subsequent record becomes
/// an object whose keys are the header names and whose values are either
/// `Value::Number` (if the field parses as `f64`) or `Value::String`.
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
}

impl CsvParser {
    /// Create a new parser with the given field delimiter.
    pub fn new(delimiter: u8) -> Self {
        Self { delimiter }
    }

    /// Create a CSV parser (comma delimiter).
    pub fn csv() -> Self {
        Self::new(b',')
    }

    /// Create a TSV parser (tab delimiter).
    pub fn tsv() -> Self {
        Self::new(b'\t')
    }
}

impl Parser for CsvParser {
    fn parse(&self, input: &str) -> Result<Value, CoreError> {
        let mut rdr = csv::ReaderBuilder::new()
            .delimiter(self.delimiter)
            .from_reader(input.as_bytes());

        let headers: Vec<String> = rdr.headers()?.iter().map(|h| h.to_owned()).collect();

        let mut rows: Vec<Value> = Vec::new();

        for result in rdr.records() {
            let record = result?;
            let mut map = Map::with_capacity(headers.len());

            for (key, field) in headers.iter().zip(record.iter()) {
                let val = if let Ok(n) = field.parse::<f64>() {
                    // Postcondition: f64 parsed successfully implies Number::from_f64 succeeds
                    // for all finite values. Infinite/NaN fields fall through to string.
                    Number::from_f64(n)
                        .map(Value::Number)
                        .unwrap_or_else(|| Value::String(field.to_owned()))
                } else {
                    Value::String(field.to_owned())
                };
                map.insert(key.clone(), val);
            }

            rows.push(Value::Object(map));
        }

        Ok(Value::Array(rows))
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
