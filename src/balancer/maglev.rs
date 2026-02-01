use std::sync::Arc;

use fnv::FnvHasher;
use parking_lot::RwLock;
use std::hash::Hasher;

use super::algorithm::Algorithm;
use super::UdpBackend;

const TABLE_SIZE: usize = 65537;

pub struct Maglev {
    backends: Vec<Arc<UdpBackend>>,
    lookup_table: RwLock<Vec<usize>>,
}

impl Maglev {
    pub fn new(backends: Vec<Arc<UdpBackend>>) -> Self {
        let maglev = Self {
            backends,
            lookup_table: RwLock::new(Vec::new()),
        };
        maglev.rebuild_table();
        maglev
    }

    pub fn rebuild_table(&self) {
        let alive_indices: Vec<usize> = self
            .backends
            .iter()
            .enumerate()
            .filter(|(_, b)| b.is_alive())
            .map(|(i, _)| i)
            .collect();

        if alive_indices.is_empty() {
            *self.lookup_table.write() = Vec::new();
            return;
        }

        let entries = self.build_weighted_entries(&alive_indices);
        if entries.is_empty() {
            *self.lookup_table.write() = Vec::new();
            return;
        }

        let table = self.populate_table(&entries);
        *self.lookup_table.write() = table;
    }

    fn build_weighted_entries(&self, alive_indices: &[usize]) -> Vec<(usize, u64, u64)> {
        let mut entries = Vec::new();

        let min_weight = alive_indices
            .iter()
            .map(|&i| self.backends[i].weight())
            .fold(f64::INFINITY, f64::min);

        if min_weight <= 0.0 {
            return entries;
        }

        for &backend_idx in alive_indices {
            let backend = &self.backends[backend_idx];
            let weight = backend.weight();
            let repetitions = (weight / min_weight).round() as usize;
            let repetitions = repetitions.max(1);

            let (offset, skip) = self.permutation(backend_idx);

            for _ in 0..repetitions {
                entries.push((backend_idx, offset, skip));
            }
        }

        entries
    }

    fn permutation(&self, backend_index: usize) -> (u64, u64) {
        let backend = &self.backends[backend_index];
        let addr_str = backend.address().to_string();
        let addr_bytes = addr_str.as_bytes();

        let mut hasher1 = FnvHasher::default();
        hasher1.write(addr_bytes);
        let h1 = hasher1.finish();

        let mut hasher2 = FnvHasher::default();
        hasher2.write(addr_bytes);
        hasher2.write(&[0xFF]);
        let h2 = hasher2.finish();

        let offset = h1 % (TABLE_SIZE as u64);
        let skip = (h2 % ((TABLE_SIZE - 1) as u64)) + 1;

        (offset, skip)
    }

    fn populate_table(&self, entries: &[(usize, u64, u64)]) -> Vec<usize> {
        let mut table = vec![usize::MAX; TABLE_SIZE];
        let mut next = vec![0u64; entries.len()];
        let mut filled = 0;

        while filled < TABLE_SIZE {
            for (entry_idx, &(backend_idx, offset, skip)) in entries.iter().enumerate() {
                let mut c = (offset + next[entry_idx] * skip) % (TABLE_SIZE as u64);

                while table[c as usize] != usize::MAX {
                    next[entry_idx] += 1;
                    c = (offset + next[entry_idx] * skip) % (TABLE_SIZE as u64);
                }

                table[c as usize] = backend_idx;
                next[entry_idx] += 1;
                filled += 1;

                if filled >= TABLE_SIZE {
                    break;
                }
            }
        }

        table
    }

    fn hash_key(&self, key: &[u8]) -> usize {
        let mut hasher = FnvHasher::default();
        hasher.write(key);
        (hasher.finish() as usize) % TABLE_SIZE
    }
}

impl Algorithm for Maglev {
    fn next(&self, key: &[u8]) -> Option<Arc<UdpBackend>> {
        let table = self.lookup_table.read();

        if table.is_empty() {
            return None;
        }

        let index = self.hash_key(key);
        let backend_idx = table[index];

        if backend_idx == usize::MAX || backend_idx >= self.backends.len() {
            return None;
        }

        let backend = &self.backends[backend_idx];

        if backend.is_alive() {
            Some(Arc::clone(backend))
        } else {
            drop(table);
            self.rebuild_table();
            let table = self.lookup_table.read();

            if table.is_empty() {
                return None;
            }

            let backend_idx = table[index];
            if backend_idx == usize::MAX || backend_idx >= self.backends.len() {
                return None;
            }

            let backend = &self.backends[backend_idx];
            if backend.is_alive() {
                Some(Arc::clone(backend))
            } else {
                None
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
            let addr: SocketAddr = format!("127.0.0.1:{}", 20000 + i).parse().unwrap();
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
            let addr: SocketAddr = format!("[::1]:{}", 20000 + i).parse().unwrap();
            let backend = UdpBackend::new(addr, SendMode::Standard, 1.0, weight)
                .await
                .unwrap();
            backends.push(Arc::new(backend));
        }
        backends
    }

    #[tokio::test]
    async fn test_maglev_consistency() {
        let backends = create_backends(&[1.0, 1.0, 1.0]).await;
        let maglev = Maglev::new(backends);

        let key = b"test_key";
        let first = maglev.next(key).unwrap().address();

        for _ in 0..100 {
            let selected = maglev.next(key).unwrap();
            assert_eq!(selected.address(), first);
        }
    }

    #[tokio::test]
    async fn test_maglev_different_keys() {
        let backends = create_backends(&[1.0, 1.0, 1.0]).await;
        let maglev = Maglev::new(backends);

        let mut results: HashMap<SocketAddr, usize> = HashMap::new();
        for i in 0..1000 {
            let key = format!("key_{}", i);
            let selected = maglev.next(key.as_bytes()).unwrap();
            *results.entry(selected.address()).or_insert(0) += 1;
        }

        assert!(results.len() > 1);
    }

    #[tokio::test]
    async fn test_maglev_single_backend() {
        let backends = create_backends(&[1.0]).await;
        let maglev = Maglev::new(backends.clone());

        for i in 0..10 {
            let key = format!("key_{}", i);
            let selected = maglev.next(key.as_bytes()).unwrap();
            assert_eq!(selected.address(), backends[0].address());
        }
    }

    #[tokio::test]
    async fn test_maglev_backend_failure_minimal_redistribution() {
        let backends = create_backends(&[1.0, 1.0, 1.0]).await;
        let maglev = Maglev::new(backends.clone());

        let mut before: HashMap<Vec<u8>, SocketAddr> = HashMap::new();
        for i in 0..100 {
            let key = format!("key_{}", i).into_bytes();
            before.insert(key.clone(), maglev.next(&key).unwrap().address());
        }

        backends[0].set_alive(false);
        maglev.rebuild_table();

        let mut redistributed = 0;
        for (key, old_addr) in &before {
            let new_addr = maglev.next(key).unwrap().address();
            if old_addr == &backends[0].address() {
                assert_ne!(new_addr, *old_addr);
                redistributed += 1;
            }
        }

        assert!(redistributed > 0);
    }

    #[tokio::test]
    async fn test_maglev_all_backends_down() {
        let backends = create_backends(&[1.0, 1.0]).await;
        let maglev = Maglev::new(backends.clone());

        backends[0].set_alive(false);
        backends[1].set_alive(false);
        maglev.rebuild_table();

        assert!(maglev.next(b"key").is_none());
    }

    #[tokio::test]
    async fn test_maglev_empty() {
        let maglev = Maglev::new(vec![]);
        assert!(maglev.next(b"key").is_none());
        assert!(maglev.backends().is_empty());
    }

    #[tokio::test]
    async fn test_maglev_weighted_distribution() {
        let backends = create_backends(&[10.0, 1.0]).await;
        let maglev = Maglev::new(backends.clone());

        let mut counts: HashMap<SocketAddr, usize> = HashMap::new();
        for i in 0..10000 {
            let key = format!("key_{}", i);
            let selected = maglev.next(key.as_bytes()).unwrap();
            *counts.entry(selected.address()).or_insert(0) += 1;
        }

        let count0 = *counts.get(&backends[0].address()).unwrap_or(&0);
        let count1 = *counts.get(&backends[1].address()).unwrap_or(&0);

        assert!(count0 > count1, "count0={} count1={}", count0, count1);
    }

    #[tokio::test]
    async fn test_maglev_len() {
        let backends = create_backends(&[1.0, 2.0, 3.0]).await;
        let maglev = Maglev::new(backends);
        assert_eq!(maglev.backends().len(), 3);
    }

    #[tokio::test]
    async fn test_maglev_consistency_ipv6() {
        let backends = create_backends_ipv6(&[1.0, 1.0, 1.0]).await;
        let maglev = Maglev::new(backends);

        let key = b"test_key";
        let first = maglev.next(key).unwrap().address();

        for _ in 0..100 {
            let selected = maglev.next(key).unwrap();
            assert_eq!(selected.address(), first);
        }
    }

    #[tokio::test]
    async fn test_maglev_different_keys_ipv6() {
        let backends = create_backends_ipv6(&[1.0, 1.0, 1.0]).await;
        let maglev = Maglev::new(backends);

        let mut results: HashMap<SocketAddr, usize> = HashMap::new();
        for i in 0..1000 {
            let key = format!("key_{}", i);
            let selected = maglev.next(key.as_bytes()).unwrap();
            *results.entry(selected.address()).or_insert(0) += 1;
        }

        assert!(results.len() > 1);
    }

    #[tokio::test]
    async fn test_maglev_backend_failure_ipv6() {
        let backends = create_backends_ipv6(&[1.0, 1.0, 1.0]).await;
        let maglev = Maglev::new(backends.clone());

        let key = b"test_key";
        let initial = maglev.next(key).unwrap().address();

        backends[0].set_alive(false);
        maglev.rebuild_table();

        let after_failure = maglev.next(key).unwrap();
        if initial == backends[0].address() {
            assert_ne!(after_failure.address(), initial);
        }
    }
}
