//! The IPC decoder must reject arbitrary bytes cleanly, and anything it accepts must re-encode
//! to a message that decodes to the same value.
#![no_main]

use fossick_proto::{Request, Response, decode, encode, read_frame};
use libfuzzer_sys::fuzz_target;

fn round_trip<T>(data: &[u8])
where
    T: serde::Serialize + for<'a> serde::Deserialize<'a> + PartialEq + std::fmt::Debug,
{
    if let Ok(msg) = decode::<T>(data) {
        let frame = encode(&msg).expect("decoded messages re-encode");
        let payload = read_frame(&mut &frame[..]).expect("own frames read back");
        let again: T = decode(&payload).expect("own payloads decode");
        assert_eq!(msg, again);
    }
}

fuzz_target!(|data: &[u8]| {
    round_trip::<Request>(data);
    round_trip::<Response>(data);
    // Framing: a length prefix followed by arbitrary bytes.
    let _ = read_frame(&mut &data[..]);
});
