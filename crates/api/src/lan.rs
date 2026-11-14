use std::net::{IpAddr, Ipv4Addr, UdpSocket};

/// Discovers the best LAN-reachable IPv4 address for this host, falling back to `local_ip_address`.
pub fn discover_lan_ip() -> Option<IpAddr> {
    if let Ok(ip) = local_ip_address::local_ip()
        && is_usable_lan_ip(ip)
    {
        return Some(ip);
    }

    if let Ok(interfaces) = local_ip_address::list_afinet_netifas() {
        let mut best: Option<(u8, IpAddr)> = None;
        for (name, ip) in interfaces {
            let IpAddr::V4(v4) = ip else {
                continue;
            };
            if !is_usable_lan_v4(v4) {
                continue;
            }
            let score = score_interface(&name, v4);
            if best.is_none_or(|(best_score, _)| score > best_score) {
                best = Some((score, ip));
            }
        }
        if let Some((_, ip)) = best {
            return Some(ip);
        }
    }

    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    let ip = socket.local_addr().ok()?.ip();
    is_usable_lan_ip(ip).then_some(ip)
}

/// Async wrapper around [`discover_lan_ip`] executed off the async runtime.
pub async fn discover_lan_ip_async() -> Option<IpAddr> {
    tokio::task::spawn_blocking(discover_lan_ip)
        .await
        .ok()
        .flatten()
}

fn is_usable_lan_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_usable_lan_v4(v4),
        IpAddr::V6(v6) => !v6.is_loopback() && !v6.is_unspecified(),
    }
}

fn is_usable_lan_v4(v4: Ipv4Addr) -> bool {
    !v4.is_loopback() && !v4.is_unspecified() && !v4.is_link_local()
}

fn score_interface(name: &str, v4: Ipv4Addr) -> u8 {
    let lower = name.to_ascii_lowercase();
    let virtual_iface = lower.starts_with("docker")
        || lower.starts_with("br-")
        || lower.starts_with("veth")
        || lower.starts_with("virbr")
        || lower.starts_with("vboxnet")
        || lower.starts_with("utun")
        || lower.starts_with("tun")
        || lower.starts_with("tap")
        || lower.starts_with("tailscale");

    let octets = v4.octets();
    let private_192 = octets[0] == 192 && octets[1] == 168;
    let private_10 = octets[0] == 10;
    let private_172 = octets[0] == 172 && (16..=31).contains(&octets[1]);

    let mut score = if virtual_iface { 10 } else { 100 };
    if private_192 {
        score += 30;
    } else if private_10 {
        score += 20;
    } else if private_172 {
        score += 10;
    }
    score
}
