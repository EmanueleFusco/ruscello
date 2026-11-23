# Ruscello dependency patch

The source is the crates.io librqbit-dht 9.1.0 release from
[rqbit commit 4309d38](https://github.com/ikatson/rqbit/tree/4309d38769ef93f6ee4f01a1ab147009a197fa8f/crates/dht).
The Apache-2.0 license and upstream copyright notice are included.

The private peer-store timestamp uses Jiff. The persisted UTC RFC 3339 JSON format stays compatible.
Review the patch when updating librqbit-dht.
