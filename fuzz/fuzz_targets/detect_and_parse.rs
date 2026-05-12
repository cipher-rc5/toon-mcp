// file: fuzz/fuzz_targets/detect_and_parse.rs
// description: Fuzz harness for FormatDetector::detect_and_parse over arbitrary bytes

#![no_main]

use libfuzzer_sys::fuzz_target;
use toon_mcp_core::FormatDetector;

fuzz_target!(|data: &[u8]| {
    // Only feed valid UTF-8 since the public API takes &str; libfuzzer will
    // discover interesting UTF-8 inputs efficiently when the input prefix
    // happens to parse.
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = FormatDetector::detect_and_parse(s);
    }
});
