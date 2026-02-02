use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use super::Packet;
use crate::balancer::{Balancer, Mirror};
use crate::metrics::{BYTES_RECEIVED, BYTES_SENT, PACKETS_RECEIVED, PACKETS_SENT};

#[cfg(target_os = "linux")]
use socket2::{Domain, Protocol, Socket, Type};

pub struct BatchListener {
    addresses: Vec<Arc<UdpSocket>>,
    address: SocketAddr,
    listener_addr_str: String,
    balancer: Option<Arc<Balancer>>,
    mirror: Option<Arc<Mirror>>,
    num_workers: usize,
}

impl BatchListener {
    pub async fn new(
        address: SocketAddr,
        num_workers: usize,
        balancer: Option<Arc<Balancer>>,
        mirror: Option<Arc<Mirror>>,
    ) -> Result<Self, std::io::Error> {
        let num_workers = if num_workers == 0 {
            num_cpus::get()
        } else {
            num_workers
        };

        let mut sockets = Vec::with_capacity(num_workers);

        for _ in 0..num_workers {
            let socket = Self::create_reuseport_socket(address)?;
            sockets.push(Arc::new(socket));
        }

        let local_addr = sockets[0].local_addr()?;
        let listener_addr_str = local_addr.to_string();

        info!(
            "Batch UDP listener started on {} with {} workers",
            local_addr, num_workers
        );

        Ok(Self {
            addresses: sockets,
            address: local_addr,
            listener_addr_str,
            balancer,
            mirror,
            num_workers,
        })
    }

    #[cfg(target_os = "linux")]
    fn create_reuseport_socket(address: SocketAddr) -> Result<UdpSocket, std::io::Error> {
        let domain = if address.is_ipv4() {
            Domain::IPV4
        } else {
            Domain::IPV6
        };

        let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
        socket.set_reuse_address(true)?;
        socket.set_reuse_port(true)?;
        socket.set_nonblocking(true)?;
        socket.bind(&address.into())?;

        let std_socket = std::net::UdpSocket::from(socket);
        UdpSocket::from_std(std_socket)
    }

    #[cfg(not(target_os = "linux"))]
    fn create_reuseport_socket(address: SocketAddr) -> Result<UdpSocket, std::io::Error> {
        use std::net::UdpSocket as StdSocket;

        let std_socket = StdSocket::bind(address)?;
        std_socket.set_nonblocking(true)?;
        UdpSocket::from_std(std_socket)
    }

    pub fn bound_addr(&self) -> SocketAddr {
        self.address
    }

    #[allow(dead_code)]
    pub fn address(&self) -> SocketAddr {
        self.address
    }

    #[allow(dead_code)]
    pub fn num_workers(&self) -> usize {
        self.num_workers
    }

    pub async fn run(self: Arc<Self>, mut shutdown_rx: mpsc::Receiver<()>) {
        let mut handles = Vec::with_capacity(self.num_workers);

        for (worker_id, socket) in self.addresses.iter().enumerate() {
            let socket = Arc::clone(socket);
            let listener = Arc::clone(&self);
            let (worker_shutdown_tx, worker_shutdown_rx) = mpsc::channel(1);

            handles.push((
                worker_shutdown_tx,
                tokio::spawn(async move {
                    listener
                        .worker_loop(worker_id, socket, worker_shutdown_rx)
                        .await;
                }),
            ));
        }

        shutdown_rx.recv().await;

        info!("Batch listener {} shutting down", self.address);

        for (shutdown_tx, _) in &handles {
            let _ = shutdown_tx.send(()).await;
        }

        for (_, handle) in handles {
            let _ = handle.await;
        }
    }

    async fn worker_loop(
        &self,
        worker_id: usize,
        socket: Arc<UdpSocket>,
        mut shutdown_rx: mpsc::Receiver<()>,
    ) {
        let mut buf = vec![0u8; 65535];

        debug!("Worker {} started for {}", worker_id, self.address);

        loop {
            tokio::select! {
                result = socket.recv_from(&mut buf) => {
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
                            let packet = Packet {
                                src_addr,
                                dst_addr: self.address,
                                payload,
                            };

                            self.handle_packet(&socket, packet).await;
                        }
                        Err(e) => {
                            error!("Worker {} error receiving packet: {}", worker_id, e);
                        }
                    }
                }
                _ = shutdown_rx.recv() => {
                    debug!("Worker {} shutting down", worker_id);
                    break;
                }
            }
        }
    }

    async fn handle_packet(&self, socket: &UdpSocket, packet: Packet) {
        if let Some(ref balancer) = self.balancer {
            match balancer
                .forward(packet.src_addr, packet.dst_addr, &packet.payload)
                .await
            {
                Ok(Some(response)) => {
                    let response_len = response.len();
                    match socket.send_to(&response, packet.src_addr).await {
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

mod num_cpus {
    pub fn get() -> usize {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn test_batch_listener_creation() {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let listener = BatchListener::new(addr, 2, None, None).await;
        assert!(listener.is_ok());

        let listener = listener.unwrap();
        assert!(listener.address().port() > 0);
        assert_eq!(listener.num_workers(), 2);
    }

    #[tokio::test]
    async fn test_batch_listener_auto_workers() {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let listener = BatchListener::new(addr, 0, None, None).await.unwrap();
        assert!(listener.num_workers() >= 1);
    }

    #[tokio::test]
    async fn test_batch_listener_receive() {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let listener = Arc::new(BatchListener::new(addr, 1, None, None).await.unwrap());
        let listener_addr = listener.address();

        let (shutdown_tx, shutdown_rx) = mpsc::channel(1);

        let listener_clone = Arc::clone(&listener);
        let handle = tokio::spawn(async move {
            listener_clone.run(shutdown_rx).await;
        });

        tokio::time::sleep(Duration::from_millis(50)).await;

        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client.send_to(b"test", listener_addr).await.unwrap();

        tokio::time::sleep(Duration::from_millis(50)).await;

        drop(shutdown_tx);
        let _ = handle.await;
    }
}
