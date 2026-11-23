# Ruscello dependency patch

Source is the crates.io librqbit 9.1.0 release from [rqbit commit 4309d38](https://github.com/ikatson/rqbit/tree/4309d38769ef93f6ee4f01a1ab147009a197fa8f/crates/librqbit), with the upstream Apache-2.0 license included.

Ruscello adds one optional session-wide peer semaphore. The existing per-torrent semaphore stays in place. The new semaphore is shared by incoming and outgoing managed peers and outgoing metadata probes. The Ruscello workspace keeps this copy outside its workspace members.

The listener can require µTP when it is explicitly selected. TCP and UDP bind conflicts share a typed port error, allowing the caller to request a new paired port or report a conflict with a fixed one.
The session's outgoing connector borrows the µTP socket from the listener task so shutdown can release the UDP port before a settings reload.

Starting an incomplete saved torrent rechecks its local pieces. A process exit can leave verified payload on disk beyond the most recent bitfield flush. A sampled hash mismatch also triggers a full check, even when the affected file was readable. This reuses the existing bitfield format and does not require a download migration.

On Windows, the recheck uses a complete sparse-file allocation map to rule out pieces whose expected hashes differ from that of an all-zero piece. It still reads and hashes matching zero pieces and any piece that overlaps allocated bytes or a file boundary. Unsupported or incomplete maps fall back to ordinary reads.
