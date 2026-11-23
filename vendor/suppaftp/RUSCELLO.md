# Ruscello dependency patch

The source is the crates.io suppaftp 12.2.1 release from
[upstream commit 18e0a88](https://github.com/veeso/suppaftp/tree/18e0a88b0047dcffbf778c59ed5900b7e19251db/crates/suppaftp).
The upstream MIT and Apache-2.0 licenses are included.

FTP modification times and directory timestamps use Jiff. The seconds used in a persisted
Ruscello FTP resume validator are unchanged. The patch leaves the transport and TLS code in place.
Review the patch when updating suppaftp.
