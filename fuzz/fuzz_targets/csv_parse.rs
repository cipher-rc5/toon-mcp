// file: fuzz/fuzz_targets/csv_parse.rs
// description: Fuzz harness for CsvParser::parse over arbitrary bytes (CSV and TSV variants)

#![no_main]

use libfuzzer_sys::fuzz_target;
use toon_mcp_core::parser::{Parser, csv::CsvParser};

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = CsvParser::csv().parse(s);
        let _ = CsvParser::tsv().parse(s);
    }
});
