use std::sync::Arc;

use super::UdpBackend;

pub trait Algorithm: Send + Sync {
    fn next(&self, key: &[u8]) -> Option<Arc<UdpBackend>>;
    fn backends(&self) -> &[Arc<UdpBackend>];
}
