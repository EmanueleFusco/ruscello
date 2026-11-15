#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| ruscello_media::fuzzing::dash(data));
