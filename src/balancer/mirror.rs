use std::net::SocketAddr;
use std::sync::Arc;

use tracing::error;

use super::UdpBackend;
use crate::metrics::{MIRROR_BYTES_SENT, MIRROR_PACKETS_FAILED, MIRROR_PACKETS_SENT};

pub struct Mirror {
    backends: Vec<Arc<UdpBackend>>,
    listener_addr: parking_lot::RwLock<String>,
}

impl Mirror {
    pub fn new(backends: Vec<Arc<UdpBackend>>, listener_addr: String) -> Self {
        Self {
            backends,
            listener_addr: parking_lot::RwLock::new(listener_addr),
        }
    }

    pub fn set_listener_addr(&self, addr: String) {
        *self.listener_addr.write() = addr;
    }

    pub async fn mirror(&self, src_addr: SocketAddr, payload: &[u8]) {
        let listener_addr = self.listener_addr.read().clone();
        for backend in &self.backends {
            let backend_str = backend.address().to_string();

            match backend.send(src_addr, payload).await {
                Ok(sent) if sent > 0 => {
                    MIRROR_PACKETS_SENT
                        .with_label_values(&[&listener_addr, &backend_str])
                        .inc();
                    MIRROR_BYTES_SENT
                        .with_label_values(&[&listener_addr, &backend_str])
                        .inc_by(sent as u64);
                }
                Ok(_) => {}
                Err(e) => {
                    MIRROR_PACKETS_FAILED
                        .with_label_values(&[&listener_addr, &backend_str])
                        .inc();
                    error!("Mirror failed to {}: {}", backend_str, e);
                }
            }
        }
    }

    pub fn backends(&self) -> &[Arc<UdpBackend>] {
        &self.backends
    }

    pub fn listener_addr(&self) -> String {
        self.listener_addr.read().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::balancer::SendMode;

    async fn create_mirror_backends() -> Vec<Arc<UdpBackend>> {
        let mut backends = Vec::new();
        for i in 0..3 {
            let addr: SocketAddr = format!("127.0.0.1:{}", 20000 + i).parse().unwrap();
            let backend = UdpBackend::new(addr, SendMode::Standard, 1.0, 1.0)
                .await
                .unwrap();
            backends.push(Arc::new(backend));
        }
        backends
    }

    #[tokio::test]
    async fn test_mirror_creation() {
        let backends = create_mirror_backends().await;
        let mirror = Mirror::new(backends.clone(), ":514".to_string());
        assert_eq!(mirror.backends().len(), 3);
    }

    #[tokio::test]
    async fn test_mirror_with_probability() {
        let addr: SocketAddr = "127.0.0.1:20001".parse().unwrap();
        let backend = UdpBackend::new(addr, SendMode::Standard, 0.5, 1.0)
            .await
            .unwrap();
        let mirror = Mirror::new(vec![Arc::new(backend)], ":514".to_string());

        let src: SocketAddr = "192.168.1.1:12345".parse().unwrap();
        for _ in 0..10 {
            mirror.mirror(src, b"test").await;
        }
    }

    #[tokio::test]
    async fn test_mirror_empty() {
        let mirror = Mirror::new(vec![], ":514".to_string());
        let src: SocketAddr = "192.168.1.1:12345".parse().unwrap();
        mirror.mirror(src, b"test").await;
    }
}
