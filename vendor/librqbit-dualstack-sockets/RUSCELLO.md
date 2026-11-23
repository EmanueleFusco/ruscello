# Ruscello dependency patch

The source is the crates.io 0.7.0 release from [upstream commit e2f221c](https://github.com/ikatson/librqbit-dualstack-sockets/tree/e2f221ca745c25c7790abb593ed260ce5a499fa1).
The upstream Apache-2.0 license is included. Review this patch when updating the dependency.

Ruscello keeps UDP receives pending through temporary macOS address and routing errors.
The socket is shared by DHT, local peer discovery, trackers and other librqbit components.
Keeping the socket does not change the selected interface or torrent state.

On Windows, `SIO_UDP_CONNRESET` disables ICMP port-unreachable reporting on each shared UDP socket.
The protocol still handles dead peers through its own timeouts and can receive from other peers on the same socket.
