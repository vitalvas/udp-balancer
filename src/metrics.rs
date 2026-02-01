use once_cell::sync::Lazy;
use prometheus::{
    register_int_counter_vec, register_int_gauge_vec, Encoder, IntCounterVec, IntGaugeVec,
    TextEncoder,
};

pub static PACKETS_RECEIVED: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "udp_balancer_packets_received_total",
        "Total number of packets received",
        &["listener"]
    )
    .expect("failed to register udp_balancer_packets_received_total")
});

pub static BYTES_RECEIVED: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "udp_balancer_bytes_received_total",
        "Total number of bytes received",
        &["listener"]
    )
    .expect("failed to register udp_balancer_bytes_received_total")
});

pub static PACKETS_SENT: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "udp_balancer_packets_sent_total",
        "Total number of packets sent (responses)",
        &["listener"]
    )
    .expect("failed to register udp_balancer_packets_sent_total")
});

pub static BYTES_SENT: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "udp_balancer_bytes_sent_total",
        "Total number of bytes sent (responses)",
        &["listener"]
    )
    .expect("failed to register udp_balancer_bytes_sent_total")
});

pub static BALANCER_PACKETS_SENT: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "udp_balancer_backend_packets_sent_total",
        "Total number of packets sent by balancer",
        &["listener", "backend"]
    )
    .expect("failed to register udp_balancer_backend_packets_sent_total")
});

pub static BALANCER_BYTES_SENT: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "udp_balancer_backend_bytes_sent_total",
        "Total number of bytes sent by balancer",
        &["listener", "backend"]
    )
    .expect("failed to register udp_balancer_backend_bytes_sent_total")
});

pub static BALANCER_PACKETS_FAILED: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "udp_balancer_backend_packets_failed_total",
        "Total number of packets that failed to send",
        &["listener", "backend"]
    )
    .expect("failed to register udp_balancer_backend_packets_failed_total")
});

pub static BALANCER_RESPONSES_RECEIVED: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "udp_balancer_backend_responses_received_total",
        "Total number of response packets received from backends",
        &["listener", "backend"]
    )
    .expect("failed to register udp_balancer_backend_responses_received_total")
});

pub static BALANCER_RESPONSE_BYTES_RECEIVED: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "udp_balancer_backend_response_bytes_received_total",
        "Total number of response bytes received from backends",
        &["listener", "backend"]
    )
    .expect("failed to register udp_balancer_backend_response_bytes_received_total")
});

pub static BALANCER_BACKEND_ALIVE: Lazy<IntGaugeVec> = Lazy::new(|| {
    register_int_gauge_vec!(
        "udp_balancer_backend_alive",
        "Whether a backend is alive (1) or dead (0)",
        &["listener", "backend"]
    )
    .expect("failed to register udp_balancer_backend_alive")
});

pub static MIRROR_PACKETS_SENT: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "udp_balancer_mirror_packets_sent_total",
        "Total number of packets sent to mirror destinations",
        &["listener", "mirror"]
    )
    .expect("failed to register udp_balancer_mirror_packets_sent_total")
});

pub static MIRROR_BYTES_SENT: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "udp_balancer_mirror_bytes_sent_total",
        "Total number of bytes sent to mirror destinations",
        &["listener", "mirror"]
    )
    .expect("failed to register udp_balancer_mirror_bytes_sent_total")
});

pub static MIRROR_PACKETS_FAILED: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "udp_balancer_mirror_packets_failed_total",
        "Total number of packets that failed to send to mirror",
        &["listener", "mirror"]
    )
    .expect("failed to register udp_balancer_mirror_packets_failed_total")
});

pub static HEALTH_CHECK_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "udp_balancer_health_check_total",
        "Total number of health checks performed",
        &["listener", "backend"]
    )
    .expect("failed to register udp_balancer_health_check_total")
});

pub static HEALTH_CHECK_FAILED: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "udp_balancer_health_check_failed_total",
        "Total number of failed health checks",
        &["listener", "backend"]
    )
    .expect("failed to register udp_balancer_health_check_failed_total")
});

pub fn init_metrics() {
    Lazy::force(&PACKETS_RECEIVED);
    Lazy::force(&BYTES_RECEIVED);
    Lazy::force(&PACKETS_SENT);
    Lazy::force(&BYTES_SENT);
    Lazy::force(&BALANCER_PACKETS_SENT);
    Lazy::force(&BALANCER_BYTES_SENT);
    Lazy::force(&BALANCER_PACKETS_FAILED);
    Lazy::force(&BALANCER_RESPONSES_RECEIVED);
    Lazy::force(&BALANCER_RESPONSE_BYTES_RECEIVED);
    Lazy::force(&BALANCER_BACKEND_ALIVE);
    Lazy::force(&MIRROR_PACKETS_SENT);
    Lazy::force(&MIRROR_BYTES_SENT);
    Lazy::force(&MIRROR_PACKETS_FAILED);
    Lazy::force(&HEALTH_CHECK_TOTAL);
    Lazy::force(&HEALTH_CHECK_FAILED);
}

pub fn gather_metrics() -> String {
    let encoder = TextEncoder::new();
    let metric_families = prometheus::gather();
    let mut buffer = Vec::new();
    encoder.encode(&metric_families, &mut buffer).unwrap();
    String::from_utf8(buffer).unwrap()
}

pub fn get_listener_stats(listener: &str) -> (u64, u64, u64, u64) {
    let rx_packets = PACKETS_RECEIVED
        .get_metric_with_label_values(&[listener])
        .map(|m| m.get())
        .unwrap_or(0);
    let rx_bytes = BYTES_RECEIVED
        .get_metric_with_label_values(&[listener])
        .map(|m| m.get())
        .unwrap_or(0);
    let tx_packets = PACKETS_SENT
        .get_metric_with_label_values(&[listener])
        .map(|m| m.get())
        .unwrap_or(0);
    let tx_bytes = BYTES_SENT
        .get_metric_with_label_values(&[listener])
        .map(|m| m.get())
        .unwrap_or(0);
    (rx_packets, rx_bytes, tx_packets, tx_bytes)
}

#[derive(Default)]
pub struct BackendStats {
    pub tx_packets: u64,
    pub tx_bytes: u64,
    pub tx_failed: u64,
    pub rx_packets: u64,
    pub rx_bytes: u64,
}

#[derive(Default)]
pub struct MirrorStats {
    pub tx_packets: u64,
    pub tx_bytes: u64,
    pub tx_failed: u64,
}

pub fn get_mirror_stats(listener: &str, mirror: &str) -> MirrorStats {
    MirrorStats {
        tx_packets: MIRROR_PACKETS_SENT
            .get_metric_with_label_values(&[listener, mirror])
            .map(|m| m.get())
            .unwrap_or(0),
        tx_bytes: MIRROR_BYTES_SENT
            .get_metric_with_label_values(&[listener, mirror])
            .map(|m| m.get())
            .unwrap_or(0),
        tx_failed: MIRROR_PACKETS_FAILED
            .get_metric_with_label_values(&[listener, mirror])
            .map(|m| m.get())
            .unwrap_or(0),
    }
}

pub fn get_backend_stats(listener: &str, backend: &str) -> BackendStats {
    BackendStats {
        tx_packets: BALANCER_PACKETS_SENT
            .get_metric_with_label_values(&[listener, backend])
            .map(|m| m.get())
            .unwrap_or(0),
        tx_bytes: BALANCER_BYTES_SENT
            .get_metric_with_label_values(&[listener, backend])
            .map(|m| m.get())
            .unwrap_or(0),
        tx_failed: BALANCER_PACKETS_FAILED
            .get_metric_with_label_values(&[listener, backend])
            .map(|m| m.get())
            .unwrap_or(0),
        rx_packets: BALANCER_RESPONSES_RECEIVED
            .get_metric_with_label_values(&[listener, backend])
            .map(|m| m.get())
            .unwrap_or(0),
        rx_bytes: BALANCER_RESPONSE_BYTES_RECEIVED
            .get_metric_with_label_values(&[listener, backend])
            .map(|m| m.get())
            .unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_packets_received_metric() {
        init_metrics();
        PACKETS_RECEIVED.with_label_values(&[":2055"]).inc();
        assert_eq!(PACKETS_RECEIVED.with_label_values(&[":2055"]).get(), 1);
    }

    #[test]
    fn test_bytes_received_metric() {
        init_metrics();
        BYTES_RECEIVED.with_label_values(&[":2055"]).inc_by(100);
        assert_eq!(BYTES_RECEIVED.with_label_values(&[":2055"]).get(), 100);
    }

    #[test]
    fn test_balancer_packets_sent_metric() {
        init_metrics();
        BALANCER_PACKETS_SENT
            .with_label_values(&[":2055", "10.0.0.1:2055"])
            .inc();
        assert_eq!(
            BALANCER_PACKETS_SENT
                .with_label_values(&[":2055", "10.0.0.1:2055"])
                .get(),
            1
        );
    }

    #[test]
    fn test_balancer_bytes_sent_metric() {
        init_metrics();
        BALANCER_BYTES_SENT
            .with_label_values(&[":2055", "10.0.0.1:2055"])
            .inc_by(512);
        assert_eq!(
            BALANCER_BYTES_SENT
                .with_label_values(&[":2055", "10.0.0.1:2055"])
                .get(),
            512
        );
    }

    #[test]
    fn test_backend_alive_gauge() {
        init_metrics();
        BALANCER_BACKEND_ALIVE
            .with_label_values(&[":2055", "10.0.0.1:2055"])
            .set(1);
        assert_eq!(
            BALANCER_BACKEND_ALIVE
                .with_label_values(&[":2055", "10.0.0.1:2055"])
                .get(),
            1
        );
        BALANCER_BACKEND_ALIVE
            .with_label_values(&[":2055", "10.0.0.1:2055"])
            .set(0);
        assert_eq!(
            BALANCER_BACKEND_ALIVE
                .with_label_values(&[":2055", "10.0.0.1:2055"])
                .get(),
            0
        );
    }

    #[test]
    fn test_mirror_metrics() {
        init_metrics();
        MIRROR_PACKETS_SENT
            .with_label_values(&[":514", "10.0.0.1:514"])
            .inc();
        MIRROR_BYTES_SENT
            .with_label_values(&[":514", "10.0.0.1:514"])
            .inc_by(256);
        MIRROR_PACKETS_FAILED
            .with_label_values(&[":514", "10.0.0.2:514"])
            .inc();

        assert_eq!(
            MIRROR_PACKETS_SENT
                .with_label_values(&[":514", "10.0.0.1:514"])
                .get(),
            1
        );
        assert_eq!(
            MIRROR_BYTES_SENT
                .with_label_values(&[":514", "10.0.0.1:514"])
                .get(),
            256
        );
        assert_eq!(
            MIRROR_PACKETS_FAILED
                .with_label_values(&[":514", "10.0.0.2:514"])
                .get(),
            1
        );
    }

    #[test]
    fn test_health_check_metrics() {
        init_metrics();
        HEALTH_CHECK_TOTAL
            .with_label_values(&[":2055", "10.0.0.1:2055"])
            .inc();
        HEALTH_CHECK_FAILED
            .with_label_values(&[":2055", "10.0.0.1:2055"])
            .inc();

        assert_eq!(
            HEALTH_CHECK_TOTAL
                .with_label_values(&[":2055", "10.0.0.1:2055"])
                .get(),
            1
        );
        assert_eq!(
            HEALTH_CHECK_FAILED
                .with_label_values(&[":2055", "10.0.0.1:2055"])
                .get(),
            1
        );
    }

    #[test]
    fn test_gather_metrics() {
        init_metrics();
        PACKETS_RECEIVED.with_label_values(&["test_gather"]).inc();
        BYTES_RECEIVED.with_label_values(&["test_gather"]).inc();
        BALANCER_PACKETS_SENT
            .with_label_values(&["test_gather", "backend"])
            .inc();
        BALANCER_BACKEND_ALIVE
            .with_label_values(&["test_gather", "backend"])
            .set(1);

        let output = gather_metrics();
        assert!(
            output.contains("udp_balancer_packets_received_total"),
            "output: {}",
            output
        );
        assert!(
            output.contains("udp_balancer_bytes_received_total"),
            "output: {}",
            output
        );
        assert!(
            output.contains("udp_balancer_backend_packets_sent_total"),
            "output: {}",
            output
        );
        assert!(
            output.contains("udp_balancer_backend_alive"),
            "output: {}",
            output
        );
    }
}
