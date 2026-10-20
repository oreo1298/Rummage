//! Fuzzes the snapshot loader with arbitrary bytes: loading must fail cleanly or produce a
//! generation that can be read without panicking.
#![no_main]

use fossick_core::persist::{Verify, load_bytes};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Checksums would reject almost every mutation, so skip them: what is fuzzed here is that
    // every accessor stays bounds-checked on arbitrary (corrupt) section contents.
    if let Ok(snap) = load_bytes(data, Verify::Unchecked) {
        let g = &snap.generation;
        for id in g.iter().take(1000) {
            let _ = g.path(id);
            let _ = g.size(id);
        }
        let _ = snap.verify_deferred();
    }
});
