#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    ruscello_http::fuzzing::checkpoint(data);
    // Recompute the CRC to exercise postcard and range validation beyond the envelope.
    if let Some((&version, body)) = data.split_first() {
        let mut framed = Vec::with_capacity(10 + body.len());
        framed.extend_from_slice(&[b'R', b'S', b'C', b'T', 1 + (version % 2), 0]);
        framed.extend_from_slice(body);
        framed.extend_from_slice(&crc32fast::hash(&framed).to_le_bytes());
        ruscello_http::fuzzing::checkpoint(&framed);
    }
});
