use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use tokio::time::timeout;
use tracing::warn;

use super::algorithm::Algorithm;
use super::session::SessionManager;
use super::{Maglev, RendezvousHashing, SendMode, UdpBackend, WeightedRoundRobin};
use crate::config::{Algorithm as ConfigAlgorithm, HashKeyField};
use crate::metrics::{
    BALANCER_BYTES_SENT, BALANCER_PACKETS_FAILED, BALANCER_PACKETS_SENT,
    BALANCER_RESPONSES_RECEIVED, BALANCER_RESPONSE_BYTES_RECEIVED,
};

pub struct Balancer {
    algorithm: Box<dyn Algorithm>,
    algorithm_type: ConfigAlgorithm,
    session_manager: Arc<SessionManager>,
    hash_key_fields: Vec<HashKeyField>,
    listener_addr: parking_lot::RwLock<String>,
    proxy_timeout: Duration,
}

impl Balancer {
    pub fn new(
        algorithm_type: ConfigAlgorithm,
        backends: Vec<Arc<UdpBackend>>,
        hash_key_fields: Vec<HashKeyField>,
        listener_addr: String,
        proxy_timeout: Duration,
    ) -> Self {
        let algorithm: Box<dyn Algorithm> = match algorithm_type {
            ConfigAlgorithm::Wrr => Box::new(WeightedRoundRobin::new(backends)),
            ConfigAlgorithm::Rh => Box::new(RendezvousHashing::new(backends)),
            ConfigAlgorithm::Maglev => Box::new(Maglev::new(backends)),
        };

        let session_manager = Arc::new(SessionManager::new(listener_addr.clone(), proxy_timeout));

        Self {
            algorithm,
            algorithm_type,
            session_manager,
            hash_key_fields,
            listener_addr: parking_lot::RwLock::new(listener_addr),
            proxy_timeout,
        }
    }

    pub fn algorithm_type(&self) -> ConfigAlgorithm {
        self.algorithm_type
    }

    pub fn set_listener_addr(&self, addr: String) {
        *self.listener_addr.write() = addr;
    }

    pub fn start_session_cleanup(&self) -> tokio::sync::mpsc::Sender<()> {
        SessionManager::start_cleanup_task(Arc::clone(&self.session_manager))
    }

    pub fn build_hash_key(
        &self,
        src_ip: IpAddr,
        dst_ip: IpAddr,
        src_port: u16,
        dst_port: u16,
        payload: &[u8],
    ) -> Vec<u8> {
        let mut key = Vec::new();

        for field in &self.hash_key_fields {
            match field {
                HashKeyField::SrcIp => match src_ip {
                    IpAddr::V4(ip) => key.extend_from_slice(&ip.octets()),
                    IpAddr::V6(ip) => key.extend_from_slice(&ip.octets()),
                },
                HashKeyField::DstIp => match dst_ip {
                    IpAddr::V4(ip) => key.extend_from_slice(&ip.octets()),
                    IpAddr::V6(ip) => key.extend_from_slice(&ip.octets()),
                },
                HashKeyField::SrcPort => {
                    key.extend_from_slice(&src_port.to_be_bytes());
                }
                HashKeyField::DstPort => {
                    key.extend_from_slice(&dst_port.to_be_bytes());
                }
                HashKeyField::Ipfix => {
                    if let Some(observation_domain_id) =
                        extract_ipfix_observation_domain_id(payload)
                    {
                        key.extend_from_slice(&observation_domain_id.to_be_bytes());
                    }
                }
                HashKeyField::IpfixTemplateId => {
                    if let Some(template_id) = extract_ipfix_template_id(payload) {
                        key.extend_from_slice(&template_id.to_be_bytes());
                    }
                }
            }
        }

        key
    }

    pub async fn forward(
        &self,
        src_addr: SocketAddr,
        dst_addr: SocketAddr,
        payload: &[u8],
    ) -> Result<Option<Vec<u8>>, String> {
        let key = self.build_hash_key(
            src_addr.ip(),
            dst_addr.ip(),
            src_addr.port(),
            dst_addr.port(),
            payload,
        );

        let backend = match self.algorithm.next(&key) {
            Some(b) => b,
            None => return Err("no available backends".to_string()),
        };

        let backend_str = backend.address().to_string();
        let listener_addr = self.listener_addr.read().clone();

        match backend.mode() {
            SendMode::Standard => {
                // Use per-client session socket for proper response correlation
                let session_socket = self
                    .session_manager
                    .get_or_create_session(src_addr, backend.address())
                    .await
                    .map_err(|e| format!("failed to create session: {}", e))?;

                match session_socket.send(payload).await {
                    Ok(sent) if sent > 0 => {
                        BALANCER_PACKETS_SENT
                            .with_label_values(&[&listener_addr, &backend_str])
                            .inc();
                        BALANCER_BYTES_SENT
                            .with_label_values(&[&listener_addr, &backend_str])
                            .inc_by(sent as u64);

                        let mut buf = vec![0u8; 65535];
                        match timeout(self.proxy_timeout, session_socket.recv(&mut buf)).await {
                            Ok(Ok(len)) if len > 0 => {
                                BALANCER_RESPONSES_RECEIVED
                                    .with_label_values(&[&listener_addr, &backend_str])
                                    .inc();
                                BALANCER_RESPONSE_BYTES_RECEIVED
                                    .with_label_values(&[&listener_addr, &backend_str])
                                    .inc_by(len as u64);
                                buf.truncate(len);
                                Ok(Some(buf))
                            }
                            Ok(Ok(_)) | Ok(Err(_)) | Err(_) => Ok(None),
                        }
                    }
                    Ok(_) => Ok(None),
                    Err(e) => {
                        BALANCER_PACKETS_FAILED
                            .with_label_values(&[&listener_addr, &backend_str])
                            .inc();
                        warn!("Failed to forward to {}: {}", backend_str, e);
                        Err(e.to_string())
                    }
                }
            }
            SendMode::PreserveSrcAddr { .. } | SendMode::FakeSrcAddr(_) => {
                // Raw socket modes - no response expected
                match backend.send(src_addr, payload).await {
                    Ok(sent) if sent > 0 => {
                        BALANCER_PACKETS_SENT
                            .with_label_values(&[&listener_addr, &backend_str])
                            .inc();
                        BALANCER_BYTES_SENT
                            .with_label_values(&[&listener_addr, &backend_str])
                            .inc_by(sent as u64);
                        Ok(None)
                    }
                    Ok(_) => Ok(None),
                    Err(e) => {
                        BALANCER_PACKETS_FAILED
                            .with_label_values(&[&listener_addr, &backend_str])
                            .inc();
                        warn!("Failed to forward to {}: {}", backend_str, e);
                        Err(e.to_string())
                    }
                }
            }
        }
    }

    pub fn backends(&self) -> &[Arc<UdpBackend>] {
        self.algorithm.backends()
    }

    pub fn listener_addr(&self) -> String {
        self.listener_addr.read().clone()
    }
}

fn extract_ipfix_observation_domain_id(payload: &[u8]) -> Option<u32> {
    if payload.len() < 16 {
        return None;
    }

    let version = u16::from_be_bytes([payload[0], payload[1]]);
    if version != 10 {
        return None;
    }

    Some(u32::from_be_bytes([
        payload[12],
        payload[13],
        payload[14],
        payload[15],
    ]))
}

fn extract_ipfix_template_id(payload: &[u8]) -> Option<u16> {
    if payload.len() < 18 {
        return None;
    }

    let version = u16::from_be_bytes([payload[0], payload[1]]);
    if version != 10 {
        return None;
    }

    let set_id = u16::from_be_bytes([payload[16], payload[17]]);

    // Set ID 256-65535 indicates a Data Set, where Set ID is the Template ID
    // Set ID 2 = Template Set, Set ID 3 = Options Template Set
    if set_id >= 256 {
        Some(set_id)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::balancer::SendMode;
    use std::net::{Ipv4Addr, Ipv6Addr};

    async fn create_backends() -> Vec<Arc<UdpBackend>> {
        let mut backends = Vec::new();
        for i in 0..3 {
            let addr: SocketAddr = format!("127.0.0.1:{}", 30000 + i).parse().unwrap();
            let backend = UdpBackend::new(addr, SendMode::Standard, 1.0, 1.0)
                .await
                .unwrap();
            backends.push(Arc::new(backend));
        }
        backends
    }

    async fn create_backends_ipv6() -> Vec<Arc<UdpBackend>> {
        let mut backends = Vec::new();
        for i in 0..3 {
            let addr: SocketAddr = format!("[::1]:{}", 30000 + i).parse().unwrap();
            let backend = UdpBackend::new(addr, SendMode::Standard, 1.0, 1.0)
                .await
                .unwrap();
            backends.push(Arc::new(backend));
        }
        backends
    }

    #[tokio::test]
    async fn test_balancer_wrr_creation() {
        let backends = create_backends().await;
        let balancer = Balancer::new(
            ConfigAlgorithm::Wrr,
            backends.clone(),
            vec![],
            ":2055".to_string(),
            Duration::from_secs(30),
        );
        assert_eq!(balancer.backends().len(), 3);
    }

    #[tokio::test]
    async fn test_balancer_rh_creation() {
        let backends = create_backends().await;
        let balancer = Balancer::new(
            ConfigAlgorithm::Rh,
            backends.clone(),
            vec![HashKeyField::SrcIp, HashKeyField::DstIp],
            ":2055".to_string(),
            Duration::from_secs(30),
        );
        assert_eq!(balancer.backends().len(), 3);
    }

    #[tokio::test]
    async fn test_balancer_maglev_creation() {
        let backends = create_backends().await;
        let balancer = Balancer::new(
            ConfigAlgorithm::Maglev,
            backends.clone(),
            vec![HashKeyField::SrcIp],
            ":2055".to_string(),
            Duration::from_secs(30),
        );
        assert_eq!(balancer.backends().len(), 3);
        assert_eq!(balancer.algorithm_type(), ConfigAlgorithm::Maglev);
    }

    #[tokio::test]
    async fn test_build_hash_key_src_ip() {
        let backends = create_backends().await;
        let balancer = Balancer::new(
            ConfigAlgorithm::Rh,
            backends,
            vec![HashKeyField::SrcIp],
            ":2055".to_string(),
            Duration::from_secs(30),
        );

        let src_ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));
        let dst_ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let key = balancer.build_hash_key(src_ip, dst_ip, 12345, 2055, &[]);

        assert_eq!(key, vec![192, 168, 1, 1]);
    }

    #[tokio::test]
    async fn test_build_hash_key_full() {
        let backends = create_backends().await;
        let balancer = Balancer::new(
            ConfigAlgorithm::Rh,
            backends,
            vec![
                HashKeyField::SrcIp,
                HashKeyField::DstIp,
                HashKeyField::SrcPort,
                HashKeyField::DstPort,
            ],
            ":2055".to_string(),
            Duration::from_secs(30),
        );

        let src_ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));
        let dst_ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let key = balancer.build_hash_key(src_ip, dst_ip, 0x3039, 0x0807, &[]);

        assert_eq!(
            key,
            vec![192, 168, 1, 1, 10, 0, 0, 1, 0x30, 0x39, 0x08, 0x07]
        );
    }

    #[tokio::test]
    async fn test_build_hash_key_src_ip_ipv6() {
        let backends = create_backends_ipv6().await;
        let balancer = Balancer::new(
            ConfigAlgorithm::Rh,
            backends,
            vec![HashKeyField::SrcIp],
            ":2055".to_string(),
            Duration::from_secs(30),
        );

        let src_ip = IpAddr::V6(Ipv6Addr::new(0x2001, 0x0db8, 0, 0, 0, 0, 0, 1));
        let dst_ip = IpAddr::V6(Ipv6Addr::new(0x2001, 0x0db8, 0, 0, 0, 0, 0, 2));
        let key = balancer.build_hash_key(src_ip, dst_ip, 12345, 2055, &[]);

        assert_eq!(
            key,
            vec![0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]
        );
    }

    #[tokio::test]
    async fn test_build_hash_key_full_ipv6() {
        let backends = create_backends_ipv6().await;
        let balancer = Balancer::new(
            ConfigAlgorithm::Rh,
            backends,
            vec![
                HashKeyField::SrcIp,
                HashKeyField::DstIp,
                HashKeyField::SrcPort,
                HashKeyField::DstPort,
            ],
            ":2055".to_string(),
            Duration::from_secs(30),
        );

        let src_ip = IpAddr::V6(Ipv6Addr::new(0x2001, 0x0db8, 0, 0, 0, 0, 0, 1));
        let dst_ip = IpAddr::V6(Ipv6Addr::new(0x2001, 0x0db8, 0, 0, 0, 0, 0, 2));
        let key = balancer.build_hash_key(src_ip, dst_ip, 0x3039, 0x0807, &[]);

        let mut expected = vec![0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
        expected.extend_from_slice(&[0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2]);
        expected.extend_from_slice(&[0x30, 0x39, 0x08, 0x07]);

        assert_eq!(key, expected);
    }

    #[tokio::test]
    async fn test_balancer_maglev_ipv6() {
        let backends = create_backends_ipv6().await;
        let balancer = Balancer::new(
            ConfigAlgorithm::Maglev,
            backends.clone(),
            vec![HashKeyField::SrcIp],
            ":2055".to_string(),
            Duration::from_secs(30),
        );
        assert_eq!(balancer.backends().len(), 3);
        assert_eq!(backends[0].address().ip(), Ipv6Addr::LOCALHOST);
    }

    #[tokio::test]
    async fn test_build_hash_key_ipfix() {
        let backends = create_backends().await;
        let balancer = Balancer::new(
            ConfigAlgorithm::Rh,
            backends,
            vec![HashKeyField::Ipfix],
            ":2055".to_string(),
            Duration::from_secs(30),
        );

        let ipfix_packet = vec![
            0x00, 0x0A, 0x00, 0x50, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x01,
        ];

        let src_ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));
        let dst_ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let key = balancer.build_hash_key(src_ip, dst_ip, 12345, 2055, &ipfix_packet);

        assert_eq!(key, vec![0x00, 0x00, 0x00, 0x01]);
    }

    #[test]
    fn test_extract_ipfix_observation_domain_id() {
        let ipfix_packet = vec![
            0x00, 0x0A, 0x00, 0x50, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xDE, 0xAD,
            0xBE, 0xEF,
        ];

        let id = extract_ipfix_observation_domain_id(&ipfix_packet);
        assert_eq!(id, Some(0xDEADBEEF));
    }

    #[test]
    fn test_extract_ipfix_invalid_version() {
        let packet = vec![
            0x00, 0x09, 0x00, 0x50, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xDE, 0xAD,
            0xBE, 0xEF,
        ];

        let id = extract_ipfix_observation_domain_id(&packet);
        assert_eq!(id, None);
    }

    #[test]
    fn test_extract_ipfix_too_short() {
        let packet = vec![0x00, 0x0A, 0x00, 0x50];
        let id = extract_ipfix_observation_domain_id(&packet);
        assert_eq!(id, None);
    }

    #[test]
    fn test_extract_ipfix_template_id() {
        // IPFIX packet with Template ID 256 (0x0100)
        let ipfix_packet = vec![
            0x00, 0x0A, // Version 10
            0x00, 0x50, // Length
            0x00, 0x00, 0x00, 0x00, // Export Time
            0x00, 0x00, 0x00, 0x00, // Sequence Number
            0x00, 0x00, 0x00, 0x01, // Observation Domain ID
            0x01, 0x00, // Set ID (Template ID 256)
            0x00, 0x10, // Set Length
        ];

        let id = extract_ipfix_template_id(&ipfix_packet);
        assert_eq!(id, Some(256));
    }

    #[test]
    fn test_extract_ipfix_template_id_high_value() {
        // IPFIX packet with Template ID 1000 (0x03E8)
        let ipfix_packet = vec![
            0x00, 0x0A, // Version 10
            0x00, 0x50, // Length
            0x00, 0x00, 0x00, 0x00, // Export Time
            0x00, 0x00, 0x00, 0x00, // Sequence Number
            0x00, 0x00, 0x00, 0x01, // Observation Domain ID
            0x03, 0xE8, // Set ID (Template ID 1000)
            0x00, 0x10, // Set Length
        ];

        let id = extract_ipfix_template_id(&ipfix_packet);
        assert_eq!(id, Some(1000));
    }

    #[test]
    fn test_extract_ipfix_template_id_template_set() {
        // IPFIX packet with Set ID 2 (Template Set, not a Data Set)
        let ipfix_packet = vec![
            0x00, 0x0A, // Version 10
            0x00, 0x50, // Length
            0x00, 0x00, 0x00, 0x00, // Export Time
            0x00, 0x00, 0x00, 0x00, // Sequence Number
            0x00, 0x00, 0x00, 0x01, // Observation Domain ID
            0x00, 0x02, // Set ID 2 (Template Set)
            0x00, 0x10, // Set Length
        ];

        let id = extract_ipfix_template_id(&ipfix_packet);
        assert_eq!(id, None);
    }

    #[test]
    fn test_extract_ipfix_template_id_too_short() {
        let packet = vec![0x00, 0x0A, 0x00, 0x50, 0x00, 0x00, 0x00, 0x00];
        let id = extract_ipfix_template_id(&packet);
        assert_eq!(id, None);
    }

    #[test]
    fn test_extract_ipfix_template_id_invalid_version() {
        // NetFlow v9 packet (version 9, not IPFIX)
        let packet = vec![
            0x00, 0x09, // Version 9
            0x00, 0x50, // Length
            0x00, 0x00, 0x00, 0x00, // Export Time
            0x00, 0x00, 0x00, 0x00, // Sequence Number
            0x00, 0x00, 0x00, 0x01, // Source ID
            0x01, 0x00, // Set ID
            0x00, 0x10, // Set Length
        ];

        let id = extract_ipfix_template_id(&packet);
        assert_eq!(id, None);
    }

    #[tokio::test]
    async fn test_build_hash_key_ipfix_template_id() {
        let backends = create_backends().await;
        let balancer = Balancer::new(
            ConfigAlgorithm::Rh,
            backends,
            vec![HashKeyField::IpfixTemplateId],
            ":2055".to_string(),
            Duration::from_secs(30),
        );

        let ipfix_packet = vec![
            0x00, 0x0A, // Version 10
            0x00, 0x50, // Length
            0x00, 0x00, 0x00, 0x00, // Export Time
            0x00, 0x00, 0x00, 0x00, // Sequence Number
            0x00, 0x00, 0x00, 0x01, // Observation Domain ID
            0x01, 0x00, // Set ID (Template ID 256)
            0x00, 0x10, // Set Length
        ];

        let src_ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));
        let dst_ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let key = balancer.build_hash_key(src_ip, dst_ip, 12345, 2055, &ipfix_packet);

        assert_eq!(key, vec![0x01, 0x00]); // Template ID 256
    }
}
