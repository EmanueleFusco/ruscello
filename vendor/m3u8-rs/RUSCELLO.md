# Ruscello dependency patch

The source is the crates.io m3u8-rs 6.0.1 release from
[upstream commit 8df664c](https://github.com/rutgersc/m3u8-rs/tree/8df664c7574a4a1b283e6d8543622c5e9842604c).
The original MIT license is included.

The program timestamp and date range tags use the time crate's offset-aware RFC 3339 support.
Variant, encryption, media segment and byte-range parsing still use the upstream parser.
Review the patch when updating m3u8-rs.
