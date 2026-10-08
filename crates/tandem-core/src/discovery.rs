//! LAN discovery: a tiny UDP broadcast "I'm here" beacon. It carries no
//! secrets — only a device id, a display name and the TCP port.

use crate::proto::*;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::mpsc::UnboundedSender;

#[derive(Clone, Debug, PartialEq)]
pub struct Discovered {
    pub id: String,
    pub name: String,
    pub addr: IpAddr,
    pub port: u16,
}

pub fn parse_beacon(data: &[u8], from: IpAddr) -> Option<Discovered> {
    let b: Beacon = postcard::from_bytes(data).ok()?;
    if b.magic != MAGIC || b.ver != PROTOCOL_VERSION || b.id.is_empty() || b.id.len() > 64 {
        return None;
    }
    // Names are shown in the UI: keep them short and printable.
    let name: String = b
        .name
        .chars()
        .filter(|c| !c.is_control())
        .take(48)
        .collect();
    Some(Discovered {
        id: b.id,
        name,
        addr: from,
        port: b.port,
    })
}

pub async fn run_beacon(id: String, name: String, tcp_port: u16, udp_port: u16) {
    let Ok(sock) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await else {
        log::warn!("discovery: cannot bind beacon socket");
        return;
    };
    let _ = sock.set_broadcast(true);
    let payload = encode(&Beacon {
        magic: MAGIC,
        ver: PROTOCOL_VERSION,
        id,
        name,
        port: tcp_port,
    });
    let target = SocketAddr::new(Ipv4Addr::BROADCAST.into(), udp_port);
    loop {
        let _ = sock.send_to(&payload, target).await;
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

pub async fn run_listener(my_id: String, udp_port: u16, tx: UnboundedSender<Discovered>) {
    let sock = match UdpSocket::bind((Ipv4Addr::UNSPECIFIED, udp_port)).await {
        Ok(s) => s,
        Err(e) => {
            log::warn!(
                "discovery: cannot listen on UDP {udp_port}: {e} (is another Tandem running?)"
            );
            return;
        }
    };
    let mut buf = [0u8; 512];
    loop {
        let Ok((n, from)) = sock.recv_from(&mut buf).await else {
            continue;
        };
        if let Some(d) = parse_beacon(&buf[..n], from.ip()) {
            if d.id != my_id && tx.send(d).is_err() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn beacon_roundtrip_and_garbage() {
        let b = encode(&Beacon {
            magic: MAGIC,
            ver: PROTOCOL_VERSION,
            id: "x".into(),
            name: "PC\n1".into(),
            port: 9,
        });
        let d = parse_beacon(&b, "10.0.0.2".parse().unwrap()).unwrap();
        assert_eq!(d.name, "PC1");
        assert!(parse_beacon(b"hello", "10.0.0.2".parse().unwrap()).is_none());
        let bad = encode(&Beacon {
            magic: *b"NOPE",
            ver: PROTOCOL_VERSION,
            id: "x".into(),
            name: "".into(),
            port: 9,
        });
        assert!(parse_beacon(&bad, "10.0.0.2".parse().unwrap()).is_none());
    }
}
