use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use super::Packet;
use crate::balancer::{Balancer, Mirror};
use crate::metrics::{BYTES_RECEIVED, BYTES_SENT, PACKETS_RECEIVED, PACKETS_SENT};

pub struct StandardListener {
    socket: Arc<UdpSocket>,
    address: SocketAddr,
    listener_addr_str: String,
    balancer: Option<Arc<Balancer>>,
    mirror: Option<Arc<Mirror>>,
}

impl StandardListener {
    pub async fn new(
        address: SocketAddr,
        balancer: Option<Arc<Balancer>>,
        mirror: Option<Arc<Mirror>>,
    ) -> Result<Self, std::io::Error> {
        let socket = UdpSocket::bind(address).await?;
        let local_addr = socket.local_addr()?;
        let listener_addr_str = local_addr.to_string();

        info!("UDP listener started on {}", local_addr);

        Ok(Self {
            socket: Arc::new(socket),
            address: local_addr,
            listener_addr_str,
            balancer,
            mirror,
        })
    }

    pub fn bound_addr(&self) -> SocketAddr {
        self.address
    }

    #[allow(dead_code)]
    pub fn address(&self) -> SocketAddr {
        self.address
    }

    #[allow(dead_code)]
    pub fn socket(&self) -> Arc<UdpSocket> {
        Arc::clone(&self.socket)
    }

    pub async fn run(self: Arc<Self>, mut shutdown_rx: mpsc::Receiver<()>) {
        let mut buf = vec![0u8; 65535];

        loop {
            tokio::select! {
                result = self.socket.recv_from(&mut buf) => {
                    match result {
                        Ok((len, src_addr)) => {
                            if len == 0 {
                                continue;
                            }

                            PACKETS_RECEIVED
                                .with_label_values(&[&self.listener_addr_str])
                                .inc();
                            BYTES_RECEIVED
                                .with_label_values(&[&self.listener_addr_str])
                                .inc_by(len as u64);

                            let payload = buf[..len].to_vec();
                            let dst_addr = self.address;
                            let packet = Packet {
                                src_addr,
                                dst_addr,
                                payload,
                            };

                            self.handle_packet(packet).await;
                        }
                        Err(e) => {
                            error!("Error receiving packet: {}", e);
                        }
                    }
                }
                _ = shutdown_rx.recv() => {
                    info!("Listener {} shutting down", self.address);
                    break;
                }
            }
        }
    }

    async fn handle_packet(&self, packet: Packet) {
        if let Some(ref balancer) = self.balancer {
            match balancer
                .forward(packet.src_addr, packet.dst_addr, &packet.payload)
                .await
            {
                Ok(Some(response)) => {
                    let response_len = response.len();
                    match self.socket.send_to(&response, packet.src_addr).await {
                        Ok(_) => {
                            PACKETS_SENT
                                .with_label_values(&[&self.listener_addr_str])
                                .inc();
                            BYTES_SENT
                                .with_label_values(&[&self.listener_addr_str])
                                .inc_by(response_len as u64);
                        }
                        Err(e) => {
                            warn!("Failed to send response to {}: {}", packet.src_addr, e);
                        }
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    warn!("Failed to forward packet: {}", e);
                }
            }
        }

        if let Some(ref mirror) = self.mirror {
            mirror.mirror(packet.src_addr, &packet.payload).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn test_listener_creation() {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let listener = StandardListener::new(addr, None, None).await;
        assert!(listener.is_ok());

        let listener = listener.unwrap();
        assert!(listener.address().port() > 0);
    }

    #[tokio::test]
    async fn test_listener_receive() {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let listener = Arc::new(StandardListener::new(addr, None, None).await.unwrap());
        let listener_addr = listener.address();

        let (shutdown_tx, shutdown_rx) = mpsc::channel(1);

        let listener_clone = Arc::clone(&listener);
        let handle = tokio::spawn(async move {
            listener_clone.run(shutdown_rx).await;
        });

        tokio::time::sleep(Duration::from_millis(10)).await;

        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client.send_to(b"test", listener_addr).await.unwrap();

        tokio::time::sleep(Duration::from_millis(10)).await;

        drop(shutdown_tx);
        let _ = handle.await;
    }

    #[tokio::test]
    async fn test_listener_socket_access() {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let listener = StandardListener::new(addr, None, None).await.unwrap();

        let socket = listener.socket();
        assert!(socket.local_addr().is_ok());
    }
}
