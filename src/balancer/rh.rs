use std::sync::Arc;

use fnv::FnvHasher;
use std::hash::Hasher;

use super::algorithm::Algorithm;
use super::UdpBackend;

pub struct RendezvousHashing {
    backends: Vec<Arc<UdpBackend>>,
}

impl RendezvousHashing {
    pub fn new(backends: Vec<Arc<UdpBackend>>) -> Self {
        Self { backends }
    }

    fn hash_with_backend(&self, key: &[u8], backend_addr: &str) -> u64 {
        let mut hasher = FnvHasher::default();
        hasher.write(key);
        hasher.write(backend_addr.as_bytes());
        hasher.finish()
    }

    fn weighted_hash(&self, hash: u64, weight: f64) -> f64 {
        let normalized = (hash as f64) / (u64::MAX as f64);
        if normalized <= 0.0 || weight <= 0.0 {
            return f64::NEG_INFINITY;
        }
        let log_uniform = -normalized.ln();
        log_uniform.powf(1.0 / weight)
    }
}

impl Algorithm for RendezvousHashing {
    fn next(&self, key: &[u8]) -> Option<Arc<UdpBackend>> {
        if self.backends.is_empty() {
            return None;
        }

        let mut best_backend: Option<&Arc<UdpBackend>> = None;
        let mut best_score = f64::NEG_INFINITY;

        for backend in &self.backends {
            if !backend.is_alive() {
                continue;
            }

            let addr_str = backend.address().to_string();
            let hash = self.hash_with_backend(key, &addr_str);
            let score = self.weighted_hash(hash, backend.weight());

            if score > best_score {
                best_score = score;
                best_backend = Some(backend);
            }
        }

        best_backend.map(Arc::clone)
    }

    fn backends(&self) -> &[Arc<UdpBackend>] {
        &self.backends
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::balancer::SendMode;
    use std::collections::HashMap;
    use std::net::SocketAddr;

    async fn create_backends(weights: &[f64]) -> Vec<Arc<UdpBackend>> {
        let mut backends = Vec::new();
        for (i, &weight) in weights.iter().enumerate() {
            let addr: SocketAddr = format!("127.0.0.1:{}", 10000 + i).parse().unwrap();
            let backend = UdpBackend::new(addr, SendMode::Standard, 1.0, weight)
                .await
                .unwrap();
            backends.push(Arc::new(backend));
        }
        backends
    }

    async fn create_backends_ipv6(weights: &[f64]) -> Vec<Arc<UdpBackend>> {
        let mut backends = Vec::new();
        for (i, &weight) in weights.iter().enumerate() {
            let addr: SocketAddr = format!("[::1]:{}", 10000 + i).parse().unwrap();
            let backend = UdpBackend::new(addr, SendMode::Standard, 1.0, weight)
                .await
                .unwrap();
            backends.push(Arc::new(backend));
        }
        backends
    }

    #[tokio::test]
    async fn test_rh_consistency() {
        let backends = create_backends(&[1.0, 1.0, 1.0]).await;
        let rh = RendezvousHashing::new(backends);

        let key = b"test_key";
        let first = rh.next(key).unwrap().address();

        for _ in 0..100 {
            let selected = rh.next(key).unwrap();
            assert_eq!(selected.address(), first);
        }
    }

    #[tokio::test]
    async fn test_rh_different_keys() {
        let backends = create_backends(&[1.0, 1.0, 1.0]).await;
        let rh = RendezvousHashing::new(backends);

        let mut results: HashMap<SocketAddr, usize> = HashMap::new();
        for i in 0..1000 {
            let key = format!("key_{}", i);
            let selected = rh.next(key.as_bytes()).unwrap();
            *results.entry(selected.address()).or_insert(0) += 1;
        }

        assert!(results.len() > 1);
    }

    #[tokio::test]
    async fn test_rh_single_backend() {
        let backends = create_backends(&[1.0]).await;
        let rh = RendezvousHashing::new(backends.clone());

        for i in 0..10 {
            let key = format!("key_{}", i);
            let selected = rh.next(key.as_bytes()).unwrap();
            assert_eq!(selected.address(), backends[0].address());
        }
    }

    #[tokio::test]
    async fn test_rh_backend_failure_minimal_redistribution() {
        let backends = create_backends(&[1.0, 1.0, 1.0]).await;
        let rh = RendezvousHashing::new(backends.clone());

        let mut before: HashMap<Vec<u8>, SocketAddr> = HashMap::new();
        for i in 0..100 {
            let key = format!("key_{}", i).into_bytes();
            before.insert(key.clone(), rh.next(&key).unwrap().address());
        }

        backends[0].set_alive(false);

        let mut redistributed = 0;
        for (key, old_addr) in &before {
            let new_addr = rh.next(key).unwrap().address();
            if old_addr == &backends[0].address() {
                assert_ne!(new_addr, *old_addr);
                redistributed += 1;
            } else {
                assert_eq!(new_addr, *old_addr);
            }
        }

        assert!(redistributed > 0);
    }

    #[tokio::test]
    async fn test_rh_all_backends_down() {
        let backends = create_backends(&[1.0, 1.0]).await;
        let rh = RendezvousHashing::new(backends.clone());

        backends[0].set_alive(false);
        backends[1].set_alive(false);

        assert!(rh.next(b"key").is_none());
    }

    #[tokio::test]
    async fn test_rh_empty() {
        let rh = RendezvousHashing::new(vec![]);
        assert!(rh.next(b"key").is_none());
        assert!(rh.backends().is_empty());
    }

    #[tokio::test]
    async fn test_rh_weighted_distribution() {
        let backends = create_backends(&[10.0, 1.0]).await;
        let rh = RendezvousHashing::new(backends.clone());

        let mut counts: HashMap<SocketAddr, usize> = HashMap::new();
        for i in 0..10000 {
            let key = format!("key_{}", i);
            let selected = rh.next(key.as_bytes()).unwrap();
            *counts.entry(selected.address()).or_insert(0) += 1;
        }

        let count0 = *counts.get(&backends[0].address()).unwrap_or(&0);
        let count1 = *counts.get(&backends[1].address()).unwrap_or(&0);

        assert!(count0 > count1, "count0={} count1={}", count0, count1);
    }

    #[tokio::test]
    async fn test_rh_len() {
        let backends = create_backends(&[1.0, 2.0, 3.0]).await;
        let rh = RendezvousHashing::new(backends);
        assert_eq!(rh.backends().len(), 3);
    }

    #[tokio::test]
    async fn test_rh_consistency_ipv6() {
        let backends = create_backends_ipv6(&[1.0, 1.0, 1.0]).await;
        let rh = RendezvousHashing::new(backends);

        let key = b"test_key";
        let first = rh.next(key).unwrap().address();

        for _ in 0..100 {
            let selected = rh.next(key).unwrap();
            assert_eq!(selected.address(), first);
        }
    }

    #[tokio::test]
    async fn test_rh_different_keys_ipv6() {
        let backends = create_backends_ipv6(&[1.0, 1.0, 1.0]).await;
        let rh = RendezvousHashing::new(backends);

        let mut results: HashMap<SocketAddr, usize> = HashMap::new();
        for i in 0..1000 {
            let key = format!("key_{}", i);
            let selected = rh.next(key.as_bytes()).unwrap();
            *results.entry(selected.address()).or_insert(0) += 1;
        }

        assert!(results.len() > 1);
    }

    #[tokio::test]
    async fn test_rh_backend_failure_ipv6() {
        let backends = create_backends_ipv6(&[1.0, 1.0, 1.0]).await;
        let rh = RendezvousHashing::new(backends.clone());

        let key = b"test_key";
        let initial = rh.next(key).unwrap().address();

        backends[0].set_alive(false);

        let after_failure = rh.next(key).unwrap();
        if initial == backends[0].address() {
            assert_ne!(after_failure.address(), initial);
        }
    }
}
