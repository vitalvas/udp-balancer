use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use super::algorithm::Algorithm;
use super::UdpBackend;

pub struct WeightedRoundRobin {
    backends: Vec<Arc<UdpBackend>>,
    weights: Vec<f64>,
    current: AtomicUsize,
    gcd_weight: f64,
    max_weight: f64,
    current_weight: parking_lot::Mutex<f64>,
}

impl WeightedRoundRobin {
    pub fn new(backends: Vec<Arc<UdpBackend>>) -> Self {
        let weights: Vec<f64> = backends.iter().map(|b| b.weight()).collect();
        let gcd = weights.iter().fold(0.0, |acc, &w| gcd_f64(acc, w));
        let max = weights.iter().cloned().fold(0.0, f64::max);

        Self {
            backends,
            weights,
            current: AtomicUsize::new(0),
            gcd_weight: if gcd > 0.0 { gcd } else { 1.0 },
            max_weight: max,
            current_weight: parking_lot::Mutex::new(0.0),
        }
    }
}

fn gcd_f64(a: f64, b: f64) -> f64 {
    let epsilon = 0.0001;
    if b < epsilon {
        return a;
    }
    gcd_f64(b, a % b)
}

impl Algorithm for WeightedRoundRobin {
    fn next(&self, _key: &[u8]) -> Option<Arc<UdpBackend>> {
        if self.backends.is_empty() {
            return None;
        }

        let alive_backends: Vec<_> = self
            .backends
            .iter()
            .enumerate()
            .filter(|(_, b)| b.is_alive())
            .collect();

        if alive_backends.is_empty() {
            return None;
        }

        if alive_backends.len() == 1 {
            return Some(Arc::clone(alive_backends[0].1));
        }

        let mut cw = self.current_weight.lock();
        let n = self.backends.len();

        loop {
            let current = self.current.fetch_add(1, Ordering::Relaxed) % n;

            if current == 0 {
                *cw -= self.gcd_weight;
                if *cw <= 0.0 {
                    *cw = self.max_weight;
                    if *cw <= 0.0 {
                        return None;
                    }
                }
            }

            if !self.backends[current].is_alive() {
                continue;
            }

            if self.weights[current] >= *cw {
                return Some(Arc::clone(&self.backends[current]));
            }
        }
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

    #[tokio::test]
    async fn test_wrr_single_backend() {
        let backends = create_backends(&[1.0]).await;
        let wrr = WeightedRoundRobin::new(backends.clone());

        for _ in 0..10 {
            let selected = wrr.next(&[]).unwrap();
            assert_eq!(selected.address(), backends[0].address());
        }
    }

    #[tokio::test]
    async fn test_wrr_equal_weights() {
        let backends = create_backends(&[1.0, 1.0, 1.0]).await;
        let wrr = WeightedRoundRobin::new(backends.clone());

        let mut counts: HashMap<SocketAddr, usize> = HashMap::new();
        for _ in 0..300 {
            let selected = wrr.next(&[]).unwrap();
            *counts.entry(selected.address()).or_insert(0) += 1;
        }

        for backend in &backends {
            let count = counts.get(&backend.address()).unwrap_or(&0);
            assert!(*count > 80 && *count < 120);
        }
    }

    #[tokio::test]
    async fn test_wrr_weighted_distribution() {
        let backends = create_backends(&[2.0, 1.0]).await;
        let wrr = WeightedRoundRobin::new(backends.clone());

        let mut counts: HashMap<SocketAddr, usize> = HashMap::new();
        for _ in 0..300 {
            let selected = wrr.next(&[]).unwrap();
            *counts.entry(selected.address()).or_insert(0) += 1;
        }

        let count0 = *counts.get(&backends[0].address()).unwrap_or(&0);
        let count1 = *counts.get(&backends[1].address()).unwrap_or(&0);

        assert!(count0 > count1);
    }

    #[tokio::test]
    async fn test_wrr_backend_failure() {
        let backends = create_backends(&[1.0, 1.0]).await;
        let wrr = WeightedRoundRobin::new(backends.clone());

        backends[0].set_alive(false);

        for _ in 0..10 {
            let selected = wrr.next(&[]).unwrap();
            assert_eq!(selected.address(), backends[1].address());
        }
    }

    #[tokio::test]
    async fn test_wrr_all_backends_down() {
        let backends = create_backends(&[1.0, 1.0]).await;
        let wrr = WeightedRoundRobin::new(backends.clone());

        backends[0].set_alive(false);
        backends[1].set_alive(false);

        assert!(wrr.next(&[]).is_none());
    }

    #[tokio::test]
    async fn test_wrr_empty() {
        let wrr = WeightedRoundRobin::new(vec![]);
        assert!(wrr.next(&[]).is_none());
        assert!(wrr.backends().is_empty());
    }

    #[tokio::test]
    async fn test_wrr_len() {
        let backends = create_backends(&[1.0, 2.0, 3.0]).await;
        let wrr = WeightedRoundRobin::new(backends);
        assert_eq!(wrr.backends().len(), 3);
    }
}
