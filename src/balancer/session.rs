use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tracing::{debug, error, warn};

use crate::metrics::{BALANCER_RESPONSES_RECEIVED, BALANCER_RESPONSE_BYTES_RECEIVED};

#[allow(dead_code)]
pub struct Session {
    pub backend_addr: SocketAddr,
    pub socket: Arc<UdpSocket>,
    pub last_activity: Instant,
}

#[allow(dead_code)]
pub struct SessionManager {
    sessions: DashMap<SessionKey, Session>,
    listener_addr: String,
    timeout: Duration,
    shutdown_tx: Option<mpsc::Sender<()>>,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct SessionKey {
    pub client_addr: SocketAddr,
    pub backend_addr: SocketAddr,
}

impl SessionManager {
    pub fn new(listener_addr: String, timeout: Duration) -> Self {
        Self {
            sessions: DashMap::new(),
            listener_addr,
            timeout,
            shutdown_tx: None,
        }
    }

    pub async fn get_or_create_session(
        &self,
        client_addr: SocketAddr,
        backend_addr: SocketAddr,
    ) -> Result<Arc<UdpSocket>, std::io::Error> {
        let key = SessionKey {
            client_addr,
            backend_addr,
        };

        // Fast path: session already exists
        if let Some(mut session) = self.sessions.get_mut(&key) {
            session.last_activity = Instant::now();
            return Ok(Arc::clone(&session.socket));
        }

        // Create new socket
        let bind_addr = if backend_addr.is_ipv4() {
            "0.0.0.0:0"
        } else {
            "[::]:0"
        };
        let socket = UdpSocket::bind(bind_addr).await?;
        socket.connect(backend_addr).await?;
        let socket = Arc::new(socket);

        let new_session = Session {
            backend_addr,
            socket: Arc::clone(&socket),
            last_activity: Instant::now(),
        };

        // Use entry API for atomic insert-if-absent
        // If another thread inserted first, use their session instead
        let entry = self.sessions.entry(key);
        match entry {
            dashmap::mapref::entry::Entry::Occupied(mut occupied) => {
                // Another thread won the race, use their socket
                occupied.get_mut().last_activity = Instant::now();
                Ok(Arc::clone(&occupied.get().socket))
            }
            dashmap::mapref::entry::Entry::Vacant(vacant) => {
                // We won, insert our session
                vacant.insert(new_session);
                Ok(socket)
            }
        }
    }

    #[allow(dead_code)]
    pub fn update_activity(&self, client_addr: SocketAddr, backend_addr: SocketAddr) {
        let key = SessionKey {
            client_addr,
            backend_addr,
        };

        if let Some(mut session) = self.sessions.get_mut(&key) {
            session.last_activity = Instant::now();
        }
    }

    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    pub fn cleanup_expired(&self) -> usize {
        let now = Instant::now();
        let mut removed = 0;

        self.sessions.retain(|_, session| {
            let expired = now.duration_since(session.last_activity) > self.timeout;
            if expired {
                removed += 1;
            }
            !expired
        });

        removed
    }

    pub fn start_cleanup_task(session_manager: Arc<Self>) -> mpsc::Sender<()> {
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel(1);
        let interval = session_manager.timeout / 2;

        tokio::spawn(async move {
            let mut interval_timer = tokio::time::interval(interval);
            interval_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                tokio::select! {
                    _ = interval_timer.tick() => {
                        let removed = session_manager.cleanup_expired();
                        if removed > 0 {
                            debug!(
                                "Cleaned up {} expired sessions, {} remaining",
                                removed,
                                session_manager.session_count()
                            );
                        }
                    }
                    _ = shutdown_rx.recv() => {
                        debug!("Session cleanup task shutting down");
                        break;
                    }
                }
            }
        });

        shutdown_tx
    }

    #[allow(dead_code)]
    pub fn start_response_forwarder(
        session_manager: Arc<Self>,
        listener_socket: Arc<UdpSocket>,
    ) -> mpsc::Sender<()> {
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel(1);
        let listener_addr = session_manager.listener_addr.clone();

        tokio::spawn(async move {
            loop {
                for session_ref in session_manager.sessions.iter() {
                    let key = session_ref.key().clone();
                    let session = session_ref.value();
                    let socket = Arc::clone(&session.socket);
                    let backend_addr = session.backend_addr;
                    let client_addr = key.client_addr;
                    let listener_socket = Arc::clone(&listener_socket);
                    let listener_addr = listener_addr.clone();

                    tokio::spawn(async move {
                        let mut buf = vec![0u8; 65535];
                        match socket.try_recv(&mut buf) {
                            Ok(len) => {
                                let backend_str = backend_addr.to_string();
                                BALANCER_RESPONSES_RECEIVED
                                    .with_label_values(&[&listener_addr, &backend_str])
                                    .inc();
                                BALANCER_RESPONSE_BYTES_RECEIVED
                                    .with_label_values(&[&listener_addr, &backend_str])
                                    .inc_by(len as u64);

                                if let Err(e) =
                                    listener_socket.send_to(&buf[..len], client_addr).await
                                {
                                    warn!("Failed to forward response to {}: {}", client_addr, e);
                                }
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                            Err(e) => {
                                error!("Error receiving from backend {}: {}", backend_addr, e);
                            }
                        }
                    });
                }

                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(1)) => {}
                    _ = shutdown_rx.recv() => {
                        debug!("Response forwarder shutting down");
                        break;
                    }
                }
            }
        });

        shutdown_tx
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_session_creation() {
        let manager = SessionManager::new(":2055".to_string(), Duration::from_secs(30));
        let client_addr: SocketAddr = "192.168.1.1:12345".parse().unwrap();
        let backend_addr: SocketAddr = "127.0.0.1:9999".parse().unwrap();

        let socket = manager
            .get_or_create_session(client_addr, backend_addr)
            .await;
        assert!(socket.is_ok());
        assert_eq!(manager.session_count(), 1);
    }

    #[tokio::test]
    async fn test_session_reuse() {
        let manager = SessionManager::new(":2055".to_string(), Duration::from_secs(30));
        let client_addr: SocketAddr = "192.168.1.1:12345".parse().unwrap();
        let backend_addr: SocketAddr = "127.0.0.1:9999".parse().unwrap();

        let socket1 = manager
            .get_or_create_session(client_addr, backend_addr)
            .await
            .unwrap();
        let socket2 = manager
            .get_or_create_session(client_addr, backend_addr)
            .await
            .unwrap();

        assert_eq!(manager.session_count(), 1);
        assert_eq!(socket1.local_addr().unwrap(), socket2.local_addr().unwrap());
    }

    #[tokio::test]
    async fn test_multiple_sessions() {
        let manager = SessionManager::new(":2055".to_string(), Duration::from_secs(30));

        let client1: SocketAddr = "192.168.1.1:12345".parse().unwrap();
        let client2: SocketAddr = "192.168.1.2:12345".parse().unwrap();
        let backend: SocketAddr = "127.0.0.1:9999".parse().unwrap();

        manager
            .get_or_create_session(client1, backend)
            .await
            .unwrap();
        manager
            .get_or_create_session(client2, backend)
            .await
            .unwrap();

        assert_eq!(manager.session_count(), 2);
    }

    #[tokio::test]
    async fn test_session_cleanup() {
        let manager = SessionManager::new(":2055".to_string(), Duration::from_millis(50));

        let client_addr: SocketAddr = "192.168.1.1:12345".parse().unwrap();
        let backend_addr: SocketAddr = "127.0.0.1:9999".parse().unwrap();

        manager
            .get_or_create_session(client_addr, backend_addr)
            .await
            .unwrap();

        assert_eq!(manager.session_count(), 1);

        tokio::time::sleep(Duration::from_millis(100)).await;

        let removed = manager.cleanup_expired();
        assert_eq!(removed, 1);
        assert_eq!(manager.session_count(), 0);
    }

    #[tokio::test]
    async fn test_session_activity_update() {
        let manager = SessionManager::new(":2055".to_string(), Duration::from_millis(100));

        let client_addr: SocketAddr = "192.168.1.1:12345".parse().unwrap();
        let backend_addr: SocketAddr = "127.0.0.1:9999".parse().unwrap();

        manager
            .get_or_create_session(client_addr, backend_addr)
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(60)).await;
        manager.update_activity(client_addr, backend_addr);

        tokio::time::sleep(Duration::from_millis(60)).await;
        let removed = manager.cleanup_expired();

        assert_eq!(removed, 0);
        assert_eq!(manager.session_count(), 1);
    }

    #[tokio::test]
    async fn test_session_key_equality() {
        let key1 = SessionKey {
            client_addr: "192.168.1.1:12345".parse().unwrap(),
            backend_addr: "10.0.0.1:2055".parse().unwrap(),
        };
        let key2 = SessionKey {
            client_addr: "192.168.1.1:12345".parse().unwrap(),
            backend_addr: "10.0.0.1:2055".parse().unwrap(),
        };
        let key3 = SessionKey {
            client_addr: "192.168.1.1:12345".parse().unwrap(),
            backend_addr: "10.0.0.2:2055".parse().unwrap(),
        };

        assert_eq!(key1, key2);
        assert_ne!(key1, key3);
    }
}
