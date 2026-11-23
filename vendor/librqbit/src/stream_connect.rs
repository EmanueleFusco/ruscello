use std::{
    net::SocketAddr,
    sync::{Arc, Weak},
    time::Duration,
};

use anyhow::{Context, bail};
use librqbit_dualstack_sockets::ConnectOpts;
use librqbit_utp::{BindDevice, UtpSocketUdp};
use serde::Serialize;

use crate::{
    Error, PeerConnectionOptions, Result,
    peer_connection::with_timeout,
    type_aliases::{BoxAsyncReadVectored, BoxAsyncWrite},
    vectored_traits::AsyncReadVectoredIntoCompat,
};

#[derive(Debug, Clone, Copy, Serialize)]
pub enum ConnectionKind {
    #[serde(rename = "tcp")]
    Tcp,
    #[serde(rename = "utp")]
    Utp,
    #[serde(rename = "socks")]
    Socks,
}

impl std::fmt::Display for ConnectionKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectionKind::Tcp => f.write_str("tcp"),
            ConnectionKind::Utp => f.write_str("uTP"),
            ConnectionKind::Socks => f.write_str("socks"),
        }
    }
}

pub struct ConnectionOptions {
    // socks5://[username:password@]host:port
    // If set, all outgoing connections will go through the proxy over TCP.
    pub proxy_url: Option<String>,
    // TCP outgoing connections are enabled by default
    pub enable_tcp: bool,
    pub peer_opts: Option<PeerConnectionOptions>,
}

impl Default for ConnectionOptions {
    fn default() -> Self {
        Self {
            enable_tcp: true,
            proxy_url: None,
            peer_opts: None,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SocksProxyConfig {
    pub host: String,
    pub port: u16,
    pub username_password: Option<(String, String)>,
}

#[derive(Default, Debug, Clone)]
pub(crate) struct StreamConnectorArgs {
    pub enable_tcp: bool,
    pub socks_proxy_config: Option<SocksProxyConfig>,
    pub utp_socket: Option<Arc<UtpSocketUdp>>,
    pub bind_device: Option<BindDevice>,
    pub ipv4_only: bool,
}

impl SocksProxyConfig {
    /// socks5h:// resolves hostnames via the proxy; socks5:// resolves them locally.
    pub fn parse(url: &str) -> anyhow::Result<Self> {
        let url = ::url::Url::parse(url).context("invalid proxy URL")?;
        match url.scheme() {
            "socks5" | "socks5h" => {}
            other => anyhow::bail!("unsupported proxy URL scheme \"{other}\""),
        }
        let host = url.host_str().context("missing host")?;
        let port = url.port().context("missing port")?;
        let up = url
            .password()
            .map(|p| (url.username().to_owned(), p.to_owned()));
        Ok(Self {
            host: host.to_owned(),
            port,
            username_password: up,
        })
    }

    async fn connect(
        &self,
        addr: SocketAddr,
    ) -> tokio_socks::Result<(
        impl tokio::io::AsyncRead + Unpin + 'static,
        impl tokio::io::AsyncWrite + Unpin + 'static,
    )> {
        let proxy_addr = (self.host.as_str(), self.port);

        let stream = if let Some((username, password)) = self.username_password.as_ref() {
            tokio_socks::tcp::Socks5Stream::connect_with_password(
                proxy_addr,
                addr,
                username.as_str(),
                password.as_str(),
            )
            .await?
        } else {
            tokio_socks::tcp::Socks5Stream::connect(proxy_addr, addr).await?
        };

        Ok(tokio::io::split(stream))
    }
}

gen_stats!(SingleStatAtomic SingleStatSnapshot, [
    attempts u64,
    successes u64,
    errors u64
], []);
gen_stats!(PerFamilyAtomic PerFamilySnapshot, [], [
    v4 SingleStatAtomic SingleStatSnapshot,
    v6 SingleStatAtomic SingleStatSnapshot
]);
gen_stats!(ConnectStatsAtomic ConnectStatsSnapshot, [], [
    socks PerFamilyAtomic PerFamilySnapshot,
    tcp PerFamilyAtomic PerFamilySnapshot,
    utp PerFamilyAtomic PerFamilySnapshot
]);

#[derive(Debug)]
pub(crate) struct StreamConnector {
    proxy_config: Option<SocksProxyConfig>,
    enable_tcp: bool,
    bind_device: Option<BindDevice>,
    // The listener task owns the socket. A stopped session must let go of its UDP port even
    // while the caller retains the old session handle to restore torrents or settings.
    utp_socket: Option<Weak<UtpSocketUdp>>,
    stats: ConnectStatsAtomic,
    ipv4_only: bool,
}

impl StreamConnector {
    pub async fn new(config: StreamConnectorArgs) -> anyhow::Result<Self> {
        #[allow(clippy::single_match)]
        match (
            config.socks_proxy_config.is_some(),
            config.enable_tcp,
            config.utp_socket.is_some(),
        ) {
            (false, false, false) => {
                bail!("no way to connect to peers, enable TCP, uTP or socks proxy")
            }
            _ => {
                // TODO: maybe validate other combinations. For now there's no way to disable TCP
            }
        }

        Ok(Self {
            proxy_config: config.socks_proxy_config,
            enable_tcp: config.enable_tcp,
            utp_socket: config.utp_socket.as_ref().map(Arc::downgrade),
            bind_device: config.bind_device,
            stats: Default::default(),
            ipv4_only: config.ipv4_only,
        })
    }

    fn get_stat(&self, kind: ConnectionKind, is_v6: bool) -> &SingleStatAtomic {
        let stat = match kind {
            ConnectionKind::Tcp => &self.stats.tcp,
            ConnectionKind::Utp => &self.stats.utp,
            ConnectionKind::Socks => &self.stats.socks,
        };
        if is_v6 { &stat.v6 } else { &stat.v4 }
    }

    async fn with_stat<R, E>(
        &self,
        kind: ConnectionKind,
        is_v6: bool,
        fut: impl Future<Output = std::result::Result<R, E>>,
    ) -> std::result::Result<R, E> {
        let stat = self.get_stat(kind, is_v6);
        stat.attempts(1);
        fut.await
            .inspect(|_| stat.successes(1))
            .inspect_err(|_| stat.errors(1))
    }

    async fn tcp_connect(
        &self,
        addr: SocketAddr,
    ) -> librqbit_dualstack_sockets::Result<tokio::net::TcpStream> {
        self.with_stat(
            ConnectionKind::Tcp,
            addr.is_ipv6(),
            librqbit_dualstack_sockets::tcp_connect(
                addr,
                ConnectOpts {
                    // Setting source port doesn't work with cloudflare warp on linux
                    // source_port: self.tcp_source_port,
                    source_port: None,
                    bind_device: self.bind_device.as_ref(),
                },
            ),
        )
        .await
    }

    pub fn stats(&self) -> &ConnectStatsAtomic {
        &self.stats
    }

    pub async fn connect<T, F>(
        &self,
        addr: SocketAddr,
        connect_timeout: Duration,
        handshake: impl Fn(BoxAsyncReadVectored, BoxAsyncWrite) -> F,
    ) -> Result<(ConnectionKind, T)>
    where
        F: Future<Output = Result<T>>,
    {
        if addr.port() == 0 {
            return Err(Error::Anyhow(anyhow::anyhow!(
                "invalid peer address (port 0): {}",
                addr
            )));
        }

        if self.ipv4_only && addr.is_ipv6() {
            return Err(Error::Anyhow(anyhow::anyhow!(
                "ipv6 disabled, skipping connection to {}",
                addr
            )));
        }

        if let Some(proxy) = self.proxy_config.as_ref() {
            let (r, w) = with_timeout("connecting", connect_timeout, async {
                Ok(self
                    .with_stat(ConnectionKind::Socks, addr.is_ipv6(), proxy.connect(addr))
                    .await?)
            })
            .await?;
            let peer = handshake(Box::new(r.into_vectored_compat()), Box::new(w)).await?;
            return Ok((ConnectionKind::Socks, peer));
        }

        let tcp_connect = async {
            let conn = with_timeout("connecting over TCP", connect_timeout, async {
                self.tcp_connect(addr).await.map_err(Error::TcpConnect)
            })
            .await?;
            let (r, w) = conn.into_split();
            let peer = handshake(Box::new(r), Box::new(w)).await?;
            Ok((ConnectionKind::Tcp, peer))
        };

        let Some(sock) = self.utp_socket.as_ref().and_then(Weak::upgrade) else {
            return if self.enable_tcp {
                tcp_connect.await
            } else {
                Err(Error::ConnectDisabled)
            };
        };
        let utp_connect = async {
            let conn = with_timeout("connecting over uTP", connect_timeout, async {
                self.with_stat(ConnectionKind::Utp, addr.is_ipv6(), sock.connect(addr))
                    .await
                    .map_err(Error::UtpConnect)
            })
            .await?;
            let (r, w) = conn.split();
            let peer = handshake(Box::new(r), Box::new(w)).await?;
            Ok((ConnectionKind::Utp, peer))
        };
        if !self.enable_tcp {
            return utp_connect.await;
        }

        // A peer can accept TCP at the socket layer and still reject BitTorrent over TCP.
        // Keep the head start for TCP and select a transport only after its peer handshake.
        tokio::pin!(tcp_connect);
        let combine_errors = |tcp, utp| Error::Connect {
            tcp: Box::new(tcp),
            utp: Box::new(utp),
        };
        tokio::select! {
            result = &mut tcp_connect => {
                return match result {
                    Ok(peer) => Ok(peer),
                    Err(tcp) => utp_connect.await.map_err(|utp| combine_errors(tcp, utp)),
                };
            }
            _ = tokio::time::sleep(Duration::from_secs(1)) => {}
        }

        tokio::pin!(utp_connect);
        tokio::select! {
            result = &mut tcp_connect => match result {
                Ok(peer) => Ok(peer),
                Err(tcp) => utp_connect.await.map_err(|utp| combine_errors(tcp, utp)),
            },
            result = &mut utp_connect => match result {
                Ok(peer) => Ok(peer),
                Err(utp) => tcp_connect.await.map_err(|tcp| combine_errors(tcp, utp)),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_socks5() {
        let c = SocksProxyConfig::parse("socks5://myhost:1234").unwrap();
        assert_eq!(c.host, "myhost");
        assert_eq!(c.port, 1234);
        assert_eq!(c.username_password, None);
    }

    #[test]
    fn test_parse_socks5h() {
        let c = SocksProxyConfig::parse("socks5h://myhost:1234").unwrap();
        assert_eq!(c.host, "myhost");
        assert_eq!(c.port, 1234);
    }

    #[test]
    fn test_parse_with_password() {
        let c = SocksProxyConfig::parse("socks5h://user:pass@myhost:1234").unwrap();
        assert_eq!(c.host, "myhost");
        assert_eq!(c.port, 1234);
        assert_eq!(
            c.username_password,
            Some(("user".to_owned(), "pass".to_owned()))
        );
    }

    #[test]
    fn test_parse_rejects_other_schemes() {
        for url in [
            "http://myhost:1234",
            "socks4://myhost:1234",
            "socks4a://myhost:1234",
        ] {
            assert!(SocksProxyConfig::parse(url).is_err(), "should reject {url}");
        }
    }

    #[test]
    fn test_parse_rejects_garbage() {
        assert!(SocksProxyConfig::parse("not a url").is_err());
        assert!(SocksProxyConfig::parse("socks5://").is_err());
    }
}
