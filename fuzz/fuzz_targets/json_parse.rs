// file: fuzz/fuzz_targets/json_parse.rs
// description: Fuzz harness for JsonParser::parse over arbitrary bytes

#![no_main]

use libfuzzer_sys::fuzz_target;
use toon_mcp_core::parser::{Parser, json::JsonParser};

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = JsonParser.parse(s);
    }
});
