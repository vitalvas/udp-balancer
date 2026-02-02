use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use rand::random;
use thiserror::Error;
use tokio::net::UdpSocket;

#[cfg(target_os = "linux")]
use crate::raw_socket::RawSocket;

#[derive(Error, Debug)]
pub enum BackendError {
    #[error("failed to create socket: {0}")]
    SocketCreation(#[from] io::Error),
    #[error("failed to send packet: {0}")]
    SendFailed(io::Error),
    #[error("raw socket not supported on this platform")]
    #[allow(dead_code)]
    RawSocketNotSupported,
}

use std::net::Ipv4Addr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendMode {
    Standard,
    PreserveSrcAddr { fallback_ipv4: Option<Ipv4Addr> },
    FakeSrcAddr(IpAddr),
}

pub struct UdpBackend {
    address: SocketAddr,
    socket: Option<UdpSocket>,
    #[cfg(target_os = "linux")]
    raw_socket: Option<RawSocket>,
    mode: SendMode,
    probability: f64,
    alive: Arc<AtomicBool>,
    weight: f64,
}

impl UdpBackend {
    #[cfg(target_os = "linux")]
    pub async fn new(
        address: SocketAddr,
        mode: SendMode,
        probability: f64,
        weight: f64,
    ) -> Result<Self, BackendError> {
        let (socket, raw_socket) = match mode {
            SendMode::Standard => {
                let bind_addr = if address.is_ipv4() {
                    "0.0.0.0:0"
                } else {
                    "[::]:0"
                };
                let socket = UdpSocket::bind(bind_addr).await?;
                socket.connect(address).await?;
                (Some(socket), None)
            }
            SendMode::PreserveSrcAddr { .. } | SendMode::FakeSrcAddr(_) => {
                let raw = if address.is_ipv4() {
                    RawSocket::new_v4()
                        .map_err(|e| BackendError::SendFailed(io::Error::other(e.to_string())))?
                } else {
                    RawSocket::new_v6()
                        .map_err(|e| BackendError::SendFailed(io::Error::other(e.to_string())))?
                };
                (None, Some(raw))
            }
        };

        Ok(Self {
            address,
            socket,
            raw_socket,
            mode,
            probability,
            alive: Arc::new(AtomicBool::new(true)),
            weight,
        })
    }

    #[cfg(not(target_os = "linux"))]
    pub async fn new(
        address: SocketAddr,
        mode: SendMode,
        probability: f64,
        weight: f64,
    ) -> Result<Self, BackendError> {
        match mode {
            SendMode::Standard => {
                let bind_addr = if address.is_ipv4() {
                    "0.0.0.0:0"
                } else {
                    "[::]:0"
                };
                let socket = UdpSocket::bind(bind_addr).await?;
                socket.connect(address).await?;
                Ok(Self {
                    address,
                    socket: Some(socket),
                    mode,
                    probability,
                    alive: Arc::new(AtomicBool::new(true)),
                    weight,
                })
            }
            SendMode::PreserveSrcAddr { .. } | SendMode::FakeSrcAddr(_) => {
                Err(BackendError::RawSocketNotSupported)
            }
        }
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }

    pub fn mode(&self) -> SendMode {
        self.mode
    }

    pub fn weight(&self) -> f64 {
        self.weight
    }

    pub fn probability(&self) -> f64 {
        self.probability
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    #[allow(dead_code)]
    pub fn set_alive(&self, alive: bool) {
        self.alive.store(alive, Ordering::Relaxed);
    }

    pub fn alive_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.alive)
    }

    pub fn should_send(&self) -> bool {
        if self.probability >= 1.0 {
            return true;
        }
        if self.probability <= 0.0 {
            return false;
        }
        random::<f64>() < self.probability
    }

    #[cfg(target_os = "linux")]
    pub async fn send(&self, src_addr: SocketAddr, payload: &[u8]) -> Result<usize, BackendError> {
        if !self.should_send() {
            return Ok(0);
        }

        match self.mode {
            SendMode::Standard => {
                if let Some(ref socket) = self.socket {
                    let sent = socket
                        .send(payload)
                        .await
                        .map_err(BackendError::SendFailed)?;
                    Ok(sent)
                } else {
                    Err(BackendError::SendFailed(io::Error::other(
                        "no socket available",
                    )))
                }
            }
            SendMode::PreserveSrcAddr { fallback_ipv4 } => {
                if let Some(ref raw) = self.raw_socket {
                    // Use fallback IPv4 when client is IPv6 but backend is IPv4
                    let effective_src = if src_addr.is_ipv6() && self.address.is_ipv4() {
                        if let Some(fallback) = fallback_ipv4 {
                            SocketAddr::new(IpAddr::V4(fallback), src_addr.port())
                        } else {
                            return Err(BackendError::SendFailed(io::Error::other(
                                "IPv6 client to IPv4 backend requires fallback_source_address",
                            )));
                        }
                    } else {
                        src_addr
                    };
                    raw.send_to(effective_src, self.address, payload)
                        .map_err(|e| BackendError::SendFailed(io::Error::other(e.to_string())))
                } else {
                    Err(BackendError::SendFailed(io::Error::other(
                        "no raw socket available",
                    )))
                }
            }
            SendMode::FakeSrcAddr(fake_ip) => {
                if let Some(ref raw) = self.raw_socket {
                    let fake_src = SocketAddr::new(fake_ip, src_addr.port());
                    raw.send_to(fake_src, self.address, payload)
                        .map_err(|e| BackendError::SendFailed(io::Error::other(e.to_string())))
                } else {
                    Err(BackendError::SendFailed(io::Error::other(
                        "no raw socket available",
                    )))
                }
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    pub async fn send(&self, _src_addr: SocketAddr, payload: &[u8]) -> Result<usize, BackendError> {
        if !self.should_send() {
            return Ok(0);
        }

        match self.mode {
            SendMode::Standard => {
                if let Some(ref socket) = self.socket {
                    let sent = socket
                        .send(payload)
                        .await
                        .map_err(BackendError::SendFailed)?;
                    Ok(sent)
                } else {
                    Err(BackendError::SendFailed(io::Error::other(
                        "no socket available",
                    )))
                }
            }
            SendMode::PreserveSrcAddr { .. } | SendMode::FakeSrcAddr(_) => {
                Err(BackendError::RawSocketNotSupported)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[tokio::test]
    async fn test_backend_standard_mode_creation() {
        let addr: SocketAddr = "127.0.0.1:9999".parse().unwrap();
        let backend = UdpBackend::new(addr, SendMode::Standard, 1.0, 1.0).await;
        assert!(backend.is_ok());
        let backend = backend.unwrap();
        assert_eq!(backend.address(), addr);
        assert!(backend.is_alive());
    }

    #[tokio::test]
    async fn test_backend_ipv6_standard() {
        let addr: SocketAddr = "[::1]:9999".parse().unwrap();
        let backend = UdpBackend::new(addr, SendMode::Standard, 1.0, 1.0).await;
        assert!(backend.is_ok());
    }

    #[tokio::test]
    async fn test_backend_alive_toggle() {
        let addr: SocketAddr = "127.0.0.1:9999".parse().unwrap();
        let backend = UdpBackend::new(addr, SendMode::Standard, 1.0, 1.0)
            .await
            .unwrap();

        assert!(backend.is_alive());
        backend.set_alive(false);
        assert!(!backend.is_alive());
        backend.set_alive(true);
        assert!(backend.is_alive());
    }

    #[tokio::test]
    async fn test_backend_weight() {
        let addr: SocketAddr = "127.0.0.1:9999".parse().unwrap();
        let backend = UdpBackend::new(addr, SendMode::Standard, 1.0, 2.5)
            .await
            .unwrap();

        assert!((backend.weight() - 2.5).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn test_backend_should_send_always() {
        let addr: SocketAddr = "127.0.0.1:9999".parse().unwrap();
        let backend = UdpBackend::new(addr, SendMode::Standard, 1.0, 1.0)
            .await
            .unwrap();

        for _ in 0..100 {
            assert!(backend.should_send());
        }
    }

    #[tokio::test]
    async fn test_backend_should_send_never() {
        let addr: SocketAddr = "127.0.0.1:9999".parse().unwrap();
        let backend = UdpBackend::new(addr, SendMode::Standard, 0.0, 1.0)
            .await
            .unwrap();

        for _ in 0..100 {
            assert!(!backend.should_send());
        }
    }

    #[tokio::test]
    async fn test_backend_should_send_probability() {
        let addr: SocketAddr = "127.0.0.1:9999".parse().unwrap();
        let backend = UdpBackend::new(addr, SendMode::Standard, 0.5, 1.0)
            .await
            .unwrap();

        let mut sent_count = 0;
        let iterations = 1000;
        for _ in 0..iterations {
            if backend.should_send() {
                sent_count += 1;
            }
        }
        let ratio = sent_count as f64 / iterations as f64;
        assert!(ratio > 0.3 && ratio < 0.7);
    }

    #[tokio::test]
    async fn test_send_mode_variants() {
        assert_eq!(SendMode::Standard, SendMode::Standard);
        assert_eq!(
            SendMode::PreserveSrcAddr {
                fallback_ipv4: None
            },
            SendMode::PreserveSrcAddr {
                fallback_ipv4: None
            }
        );
        assert_eq!(
            SendMode::PreserveSrcAddr {
                fallback_ipv4: Some(Ipv4Addr::new(192, 0, 2, 1))
            },
            SendMode::PreserveSrcAddr {
                fallback_ipv4: Some(Ipv4Addr::new(192, 0, 2, 1))
            }
        );

        let fake_ip = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1));
        assert_eq!(
            SendMode::FakeSrcAddr(fake_ip),
            SendMode::FakeSrcAddr(fake_ip)
        );
    }

    #[tokio::test]
    async fn test_backend_alive_flag_shared() {
        let addr: SocketAddr = "127.0.0.1:9999".parse().unwrap();
        let backend = UdpBackend::new(addr, SendMode::Standard, 1.0, 1.0)
            .await
            .unwrap();

        let flag = backend.alive_flag();
        assert!(flag.load(Ordering::Relaxed));

        backend.set_alive(false);
        assert!(!flag.load(Ordering::Relaxed));
    }

    #[cfg(not(target_os = "linux"))]
    #[tokio::test]
    async fn test_raw_socket_not_supported() {
        let addr: SocketAddr = "127.0.0.1:9999".parse().unwrap();
        let result = UdpBackend::new(
            addr,
            SendMode::PreserveSrcAddr {
                fallback_ipv4: None,
            },
            1.0,
            1.0,
        )
        .await;
        assert!(matches!(result, Err(BackendError::RawSocketNotSupported)));
    }
}
