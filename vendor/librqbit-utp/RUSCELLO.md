# Ruscello dependency patch

Source is the crates.io librqbit-utp 0.7.0 release from [upstream commit c26f57b](https://github.com/ikatson/librqbit-utp/tree/c26f57b2debbe35ed0ace1ad419de529f7a5bf95). The Apache-2.0 license is included.

Outgoing connection attempts resend unacknowledged SYN packets after successive intervals of 1, 2 and 4 seconds. Each resend retains the connection identifier and sequence number. ACK handling checks both fields and skips the ambiguous initial RTT sample after a resend. The caller retains its existing deadline and cancellation behavior. On reception, a repeated SYN does not create a second pending or active stream. The behavior follows [BEP 29](https://www.bittorrent.org/beps/bep_0029.html).
