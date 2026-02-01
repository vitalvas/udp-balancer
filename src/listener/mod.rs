pub mod batch;
pub mod standard;

pub use batch::BatchListener;
pub use standard::StandardListener;

use std::net::{IpAddr, SocketAddr};

#[derive(Clone, Debug)]
pub struct Packet {
    pub src_addr: SocketAddr,
    pub dst_addr: SocketAddr,
    pub payload: Vec<u8>,
}

#[allow(dead_code)]
impl Packet {
    pub fn new(src_addr: SocketAddr, dst_ip: IpAddr, dst_port: u16, payload: Vec<u8>) -> Self {
        Self {
            src_addr,
            dst_addr: SocketAddr::new(dst_ip, dst_port),
            payload,
        }
    }

    pub fn src_ip(&self) -> IpAddr {
        self.src_addr.ip()
    }

    pub fn src_port(&self) -> u16 {
        self.src_addr.port()
    }

    pub fn dst_ip(&self) -> IpAddr {
        self.dst_addr.ip()
    }

    pub fn dst_port(&self) -> u16 {
        self.dst_addr.port()
    }

    pub fn len(&self) -> usize {
        self.payload.len()
    }

    pub fn is_empty(&self) -> bool {
        self.payload.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn test_packet_creation() {
        let src_addr: SocketAddr = "192.168.1.1:12345".parse().unwrap();
        let dst_ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let payload = b"test data".to_vec();

        let packet = Packet::new(src_addr, dst_ip, 2055, payload.clone());

        assert_eq!(packet.src_addr, src_addr);
        assert_eq!(packet.src_ip(), IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)));
        assert_eq!(packet.src_port(), 12345);
        assert_eq!(packet.dst_ip(), dst_ip);
        assert_eq!(packet.dst_port(), 2055);
        assert_eq!(packet.payload, payload);
        assert_eq!(packet.len(), 9);
        assert!(!packet.is_empty());
    }

    #[test]
    fn test_packet_empty() {
        let src_addr: SocketAddr = "192.168.1.1:12345".parse().unwrap();
        let dst_ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));

        let packet = Packet::new(src_addr, dst_ip, 2055, vec![]);

        assert!(packet.is_empty());
        assert_eq!(packet.len(), 0);
    }
}
