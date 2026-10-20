//! Fuzzes the query parser, printer and compiler: nothing may panic, and printing a parsed
//! query and parsing it again must give the same tree.
#![no_main]

use fossick_core::query::{
    Clock, CompileContext, Macros, NumericIds, ParseOptions, SearchOptions, compile_str, parse,
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };
    let opts = ParseOptions::default();
    let tree = parse(s, &opts);
    let printed = tree.to_string();
    let again = parse(&printed, &opts);
    assert_eq!(tree, again, "round trip failed for {s:?} -> {printed:?}");
    let macros = Macros::default();
    let ctx = CompileContext {
        options: SearchOptions::default(),
        clock: Clock::at(1_718_206_200, jiff::tz::TimeZone::UTC),
        macros: &macros,
        users: &NumericIds,
    };
    let _ = compile_str(s, &ctx);
});
