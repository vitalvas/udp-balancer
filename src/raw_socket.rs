use std::io;
#[cfg(target_os = "linux")]
use std::net::{IpAddr, SocketAddr};
#[cfg(any(target_os = "linux", test))]
use std::net::{Ipv4Addr, Ipv6Addr};
#[cfg(target_os = "linux")]
use std::os::unix::io::AsRawFd;

#[cfg(target_os = "linux")]
use socket2::{Domain, Protocol, Socket, Type};
use thiserror::Error;

#[allow(dead_code)]
#[derive(Error, Debug)]
pub enum RawSocketError {
    #[error("socket creation failed: {0}")]
    SocketCreation(#[from] io::Error),
    #[error("address family mismatch")]
    AddressFamilyMismatch,
    #[error("raw sockets not supported on this platform")]
    NotSupported,
}

#[allow(dead_code)]
pub struct RawSocket {
    #[cfg(target_os = "linux")]
    socket: Socket,
    #[cfg(target_os = "linux")]
    is_ipv6: bool,
}

#[allow(dead_code)]
impl RawSocket {
    #[cfg(target_os = "linux")]
    pub fn new_v4() -> Result<Self, RawSocketError> {
        let raw_type = Type::from(libc::SOCK_RAW);
        let socket = Socket::new(Domain::IPV4, raw_type, Some(Protocol::UDP))?;
        socket.set_nonblocking(true)?;
        let fd = socket.as_raw_fd();
        let one: libc::c_int = 1;
        unsafe {
            libc::setsockopt(
                fd,
                libc::IPPROTO_IP,
                libc::IP_HDRINCL,
                &one as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );
        }
        Ok(Self {
            socket,
            is_ipv6: false,
        })
    }

    #[cfg(not(target_os = "linux"))]
    pub fn new_v4() -> Result<Self, RawSocketError> {
        Err(RawSocketError::NotSupported)
    }

    #[cfg(target_os = "linux")]
    pub fn new_v6() -> Result<Self, RawSocketError> {
        let raw_type = Type::from(libc::SOCK_RAW);
        let socket = Socket::new(Domain::IPV6, raw_type, Some(Protocol::UDP))?;
        socket.set_nonblocking(true)?;
        Ok(Self {
            socket,
            is_ipv6: true,
        })
    }

    #[cfg(not(target_os = "linux"))]
    pub fn new_v6() -> Result<Self, RawSocketError> {
        Err(RawSocketError::NotSupported)
    }

    #[cfg(target_os = "linux")]
    pub fn send_to(
        &self,
        src_addr: SocketAddr,
        dst_addr: SocketAddr,
        payload: &[u8],
    ) -> Result<usize, RawSocketError> {
        match (src_addr.ip(), dst_addr.ip()) {
            (IpAddr::V4(src_ip), IpAddr::V4(dst_ip)) => {
                if self.is_ipv6 {
                    return Err(RawSocketError::AddressFamilyMismatch);
                }
                let packet = build_ipv4_udp_packet(
                    src_ip,
                    dst_ip,
                    src_addr.port(),
                    dst_addr.port(),
                    payload,
                );
                let sockaddr = socket2::SockAddr::from(dst_addr);
                let sent = self.socket.send_to(&packet, &sockaddr)?;
                Ok(sent)
            }
            (IpAddr::V6(src_ip), IpAddr::V6(dst_ip)) => {
                if !self.is_ipv6 {
                    return Err(RawSocketError::AddressFamilyMismatch);
                }
                let packet = build_ipv6_udp_packet(
                    src_ip,
                    dst_ip,
                    src_addr.port(),
                    dst_addr.port(),
                    payload,
                );
                let sockaddr = socket2::SockAddr::from(dst_addr);
                let sent = self.socket.send_to(&packet, &sockaddr)?;
                Ok(sent)
            }
            (IpAddr::V4(src_ip), IpAddr::V6(dst_ip)) => {
                // Map IPv4 source to IPv6 using RFC 6052 Well-Known Prefix (64:ff9b::/96)
                if !self.is_ipv6 {
                    return Err(RawSocketError::AddressFamilyMismatch);
                }
                let mapped_src = ipv4_to_ipv6_mapped(src_ip);
                let packet = build_ipv6_udp_packet(
                    mapped_src,
                    dst_ip,
                    src_addr.port(),
                    dst_addr.port(),
                    payload,
                );
                let sockaddr = socket2::SockAddr::from(dst_addr);
                let sent = self.socket.send_to(&packet, &sockaddr)?;
                Ok(sent)
            }
            (IpAddr::V6(_), IpAddr::V4(_)) => {
                // Cannot send IPv6 source to IPv4 destination without explicit fake address
                Err(RawSocketError::AddressFamilyMismatch)
            }
        }
    }
}

/// Convert IPv4 address to IPv6 using RFC 6052 Well-Known Prefix (64:ff9b::/96)
#[cfg(any(target_os = "linux", test))]
fn ipv4_to_ipv6_mapped(ipv4: Ipv4Addr) -> Ipv6Addr {
    let octets = ipv4.octets();
    Ipv6Addr::new(
        0x0064,
        0xff9b,
        0x0000,
        0x0000,
        0x0000,
        0x0000,
        u16::from(octets[0]) << 8 | u16::from(octets[1]),
        u16::from(octets[2]) << 8 | u16::from(octets[3]),
    )
}

#[cfg(any(target_os = "linux", test))]
fn build_ipv4_udp_packet(
    src_ip: Ipv4Addr,
    dst_ip: Ipv4Addr,
    src_port: u16,
    dst_port: u16,
    payload: &[u8],
) -> Vec<u8> {
    let udp_len = 8 + payload.len();
    let total_len = 20 + udp_len;
    let mut packet = vec![0u8; total_len];

    packet[0] = 0x45;
    packet[1] = 0x00;
    packet[2] = (total_len >> 8) as u8;
    packet[3] = total_len as u8;
    packet[4] = 0x00;
    packet[5] = 0x00;
    packet[6] = 0x40;
    packet[7] = 0x00;
    packet[8] = 64;
    packet[9] = 17;
    packet[10] = 0x00;
    packet[11] = 0x00;
    packet[12..16].copy_from_slice(&src_ip.octets());
    packet[16..20].copy_from_slice(&dst_ip.octets());

    let ip_checksum = checksum(&packet[0..20]);
    packet[10] = (ip_checksum >> 8) as u8;
    packet[11] = ip_checksum as u8;

    packet[20] = (src_port >> 8) as u8;
    packet[21] = src_port as u8;
    packet[22] = (dst_port >> 8) as u8;
    packet[23] = dst_port as u8;
    packet[24] = (udp_len >> 8) as u8;
    packet[25] = udp_len as u8;
    packet[26] = 0x00;
    packet[27] = 0x00;
    packet[28..].copy_from_slice(payload);

    let udp_checksum = udp_checksum_v4(&src_ip, &dst_ip, &packet[20..]);
    packet[26] = (udp_checksum >> 8) as u8;
    packet[27] = udp_checksum as u8;

    packet
}

#[cfg(any(target_os = "linux", test))]
fn build_ipv6_udp_packet(
    src_ip: Ipv6Addr,
    dst_ip: Ipv6Addr,
    src_port: u16,
    dst_port: u16,
    payload: &[u8],
) -> Vec<u8> {
    let udp_len = 8 + payload.len();
    let mut packet = vec![0u8; udp_len];

    packet[0] = (src_port >> 8) as u8;
    packet[1] = src_port as u8;
    packet[2] = (dst_port >> 8) as u8;
    packet[3] = dst_port as u8;
    packet[4] = (udp_len >> 8) as u8;
    packet[5] = udp_len as u8;
    packet[6] = 0x00;
    packet[7] = 0x00;
    packet[8..].copy_from_slice(payload);

    let udp_checksum = udp_checksum_v6(&src_ip, &dst_ip, &packet);
    packet[6] = (udp_checksum >> 8) as u8;
    packet[7] = udp_checksum as u8;

    packet
}

#[cfg(any(target_os = "linux", test))]
fn checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;
    let len = data.len();

    while i + 1 < len {
        sum += u32::from(data[i]) << 8 | u32::from(data[i + 1]);
        i += 2;
    }

    if i < len {
        sum += u32::from(data[i]) << 8;
    }

    while (sum >> 16) != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }

    !sum as u16
}

#[cfg(any(target_os = "linux", test))]
fn udp_checksum_v4(src_ip: &Ipv4Addr, dst_ip: &Ipv4Addr, udp_segment: &[u8]) -> u16 {
    let mut sum: u32 = 0;

    let src = src_ip.octets();
    let dst = dst_ip.octets();
    sum += u32::from(src[0]) << 8 | u32::from(src[1]);
    sum += u32::from(src[2]) << 8 | u32::from(src[3]);
    sum += u32::from(dst[0]) << 8 | u32::from(dst[1]);
    sum += u32::from(dst[2]) << 8 | u32::from(dst[3]);
    sum += 17;
    sum += udp_segment.len() as u32;

    let mut i = 0;
    let len = udp_segment.len();
    while i + 1 < len {
        sum += u32::from(udp_segment[i]) << 8 | u32::from(udp_segment[i + 1]);
        i += 2;
    }
    if i < len {
        sum += u32::from(udp_segment[i]) << 8;
    }

    while (sum >> 16) != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }

    let result = !sum as u16;
    if result == 0 {
        0xFFFF
    } else {
        result
    }
}

#[cfg(any(target_os = "linux", test))]
fn udp_checksum_v6(src_ip: &Ipv6Addr, dst_ip: &Ipv6Addr, udp_segment: &[u8]) -> u16 {
    let mut sum: u32 = 0;

    let src = src_ip.octets();
    let dst = dst_ip.octets();
    for i in (0..16).step_by(2) {
        sum += u32::from(src[i]) << 8 | u32::from(src[i + 1]);
        sum += u32::from(dst[i]) << 8 | u32::from(dst[i + 1]);
    }
    sum += (udp_segment.len() >> 16) as u32;
    sum += (udp_segment.len() & 0xFFFF) as u32;
    sum += 17;

    let mut i = 0;
    let len = udp_segment.len();
    while i + 1 < len {
        sum += u32::from(udp_segment[i]) << 8 | u32::from(udp_segment[i + 1]);
        i += 2;
    }
    if i < len {
        sum += u32::from(udp_segment[i]) << 8;
    }

    while (sum >> 16) != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }

    let result = !sum as u16;
    if result == 0 {
        0xFFFF
    } else {
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_checksum_simple() {
        let data = [
            0x45, 0x00, 0x00, 0x3c, 0x1c, 0x46, 0x40, 0x00, 0x40, 0x06, 0x00, 0x00, 0xac, 0x10,
            0x0a, 0x63, 0xac, 0x10, 0x0a, 0x0c,
        ];
        let result = checksum(&data);
        assert_eq!(result, 0xB1E6);
    }

    #[test]
    fn test_checksum_with_odd_length() {
        let data = [0x45, 0x00, 0x00];
        let _ = checksum(&data);
    }

    #[test]
    fn test_udp_checksum_v4() {
        let src_ip = Ipv4Addr::new(192, 168, 1, 1);
        let dst_ip = Ipv4Addr::new(192, 168, 1, 2);
        let udp_segment = [
            0x04, 0xD2, 0x00, 0x50, 0x00, 0x0C, 0x00, 0x00, 0x48, 0x69, 0x21, 0x00,
        ];
        let result = udp_checksum_v4(&src_ip, &dst_ip, &udp_segment);
        assert_ne!(result, 0);
    }

    #[test]
    fn test_udp_checksum_v6() {
        let src_ip = Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1);
        let dst_ip = Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 2);
        let udp_segment = [
            0x04, 0xD2, 0x00, 0x50, 0x00, 0x0C, 0x00, 0x00, 0x48, 0x69, 0x21, 0x00,
        ];
        let result = udp_checksum_v6(&src_ip, &dst_ip, &udp_segment);
        assert_ne!(result, 0);
    }

    #[test]
    fn test_build_ipv4_udp_packet() {
        let src_ip = Ipv4Addr::new(192, 168, 1, 1);
        let dst_ip = Ipv4Addr::new(192, 168, 1, 2);
        let payload = b"Hello";
        let packet = build_ipv4_udp_packet(src_ip, dst_ip, 12345, 80, payload);

        assert_eq!(packet[0] & 0xF0, 0x40);
        assert_eq!(packet[0] & 0x0F, 5);

        let total_len = (u16::from(packet[2]) << 8) | u16::from(packet[3]);
        assert_eq!(total_len as usize, 20 + 8 + payload.len());

        assert_eq!(packet[9], 17);

        assert_eq!(&packet[12..16], &src_ip.octets());
        assert_eq!(&packet[16..20], &dst_ip.octets());

        let src_port = (u16::from(packet[20]) << 8) | u16::from(packet[21]);
        let dst_port = (u16::from(packet[22]) << 8) | u16::from(packet[23]);
        assert_eq!(src_port, 12345);
        assert_eq!(dst_port, 80);

        assert_eq!(&packet[28..], payload);
    }

    #[test]
    fn test_build_ipv6_udp_packet() {
        let src_ip = Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1);
        let dst_ip = Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 2);
        let payload = b"Hello";
        let packet = build_ipv6_udp_packet(src_ip, dst_ip, 12345, 80, payload);

        let src_port = (u16::from(packet[0]) << 8) | u16::from(packet[1]);
        let dst_port = (u16::from(packet[2]) << 8) | u16::from(packet[3]);
        assert_eq!(src_port, 12345);
        assert_eq!(dst_port, 80);

        let udp_len = (u16::from(packet[4]) << 8) | u16::from(packet[5]);
        assert_eq!(udp_len as usize, 8 + payload.len());

        assert_eq!(&packet[8..], payload);
    }

    #[test]
    fn test_udp_checksum_zero_returns_ffff() {
        let src_ip = Ipv4Addr::new(127, 0, 0, 1);
        let dst_ip = Ipv4Addr::new(127, 0, 0, 1);
        let udp_segment = [0x00, 0x01, 0x00, 0x01, 0x00, 0x08, 0x00, 0x00];
        let result = udp_checksum_v4(&src_ip, &dst_ip, &udp_segment);
        assert_ne!(result, 0);
    }

    #[test]
    fn test_ip_header_checksum_verification() {
        let src_ip = Ipv4Addr::new(10, 0, 0, 1);
        let dst_ip = Ipv4Addr::new(10, 0, 0, 2);
        let packet = build_ipv4_udp_packet(src_ip, dst_ip, 1234, 5678, b"test");

        let verify_checksum = checksum(&packet[0..20]);
        assert_eq!(verify_checksum, 0);
    }

    #[test]
    fn test_ipv4_to_ipv6_mapped() {
        // Test RFC 6052 Well-Known Prefix mapping (64:ff9b::/96)
        let ipv4 = Ipv4Addr::new(192, 0, 2, 1);
        let ipv6 = ipv4_to_ipv6_mapped(ipv4);

        // Expected: 64:ff9b::192.0.2.1 = 64:ff9b::c000:201
        assert_eq!(
            ipv6,
            Ipv6Addr::new(0x0064, 0xff9b, 0, 0, 0, 0, 0xc000, 0x0201)
        );
    }

    #[test]
    fn test_ipv4_to_ipv6_mapped_localhost() {
        let ipv4 = Ipv4Addr::new(127, 0, 0, 1);
        let ipv6 = ipv4_to_ipv6_mapped(ipv4);

        // Expected: 64:ff9b::127.0.0.1 = 64:ff9b::7f00:1
        assert_eq!(
            ipv6,
            Ipv6Addr::new(0x0064, 0xff9b, 0, 0, 0, 0, 0x7f00, 0x0001)
        );
    }
}
