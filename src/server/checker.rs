use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use reqwest::Client;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::metrics::{BALANCER_BACKEND_ALIVE, HEALTH_CHECK_FAILED, HEALTH_CHECK_TOTAL};

#[derive(Clone)]
pub struct HealthCheck {
    pub url: String,
    pub interval: Duration,
    pub timeout: Duration,
    pub alive: Arc<AtomicBool>,
    pub listener_addr: String,
    pub backend_addr: String,
}

pub struct HealthChecker {
    checks: Vec<HealthCheck>,
    client: Client,
}

impl HealthChecker {
    pub fn new(checks: Vec<HealthCheck>) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .danger_accept_invalid_certs(true)
            .build()
            .expect("Failed to create HTTP client");

        Self { checks, client }
    }

    pub async fn run(self, mut shutdown_rx: mpsc::Receiver<()>) {
        if self.checks.is_empty() {
            debug!("No health checks configured");
            shutdown_rx.recv().await;
            return;
        }

        let mut handles = Vec::new();

        for check in self.checks {
            let client = self.client.clone();
            let (check_shutdown_tx, check_shutdown_rx) = mpsc::channel(1);

            handles.push((
                check_shutdown_tx,
                tokio::spawn(async move {
                    Self::run_check(client, check, check_shutdown_rx).await;
                }),
            ));
        }

        shutdown_rx.recv().await;

        info!("Health checker shutting down");

        for (shutdown_tx, _) in &handles {
            let _ = shutdown_tx.send(()).await;
        }

        for (_, handle) in handles {
            let _ = handle.await;
        }
    }

    async fn run_check(client: Client, check: HealthCheck, mut shutdown_rx: mpsc::Receiver<()>) {
        let mut interval = tokio::time::interval(check.interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let mut was_alive = true;

        BALANCER_BACKEND_ALIVE
            .with_label_values(&[&check.listener_addr, &check.backend_addr])
            .set(1);

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let result = Self::perform_check(&client, &check.url, check.timeout).await;

                    HEALTH_CHECK_TOTAL
                        .with_label_values(&[&check.listener_addr, &check.backend_addr])
                        .inc();

                    let is_alive = result.is_ok();

                    if is_alive != was_alive {
                        if is_alive {
                            info!(
                                "Backend {} is now healthy",
                                check.backend_addr
                            );
                        } else {
                            warn!(
                                "Backend {} is now unhealthy: {}",
                                check.backend_addr,
                                result.unwrap_err()
                            );
                        }
                        was_alive = is_alive;
                    }

                    if !is_alive {
                        HEALTH_CHECK_FAILED
                            .with_label_values(&[&check.listener_addr, &check.backend_addr])
                            .inc();
                    }

                    check.alive.store(is_alive, std::sync::atomic::Ordering::Relaxed);
                    BALANCER_BACKEND_ALIVE
                        .with_label_values(&[&check.listener_addr, &check.backend_addr])
                        .set(if is_alive { 1 } else { 0 });
                }
                _ = shutdown_rx.recv() => {
                    debug!("Health check for {} stopping", check.backend_addr);
                    break;
                }
            }
        }
    }

    async fn perform_check(client: &Client, url: &str, timeout: Duration) -> Result<(), String> {
        let result = client.get(url).timeout(timeout).send().await;

        match result {
            Ok(response) => {
                let status = response.status().as_u16();
                if (200..500).contains(&status) {
                    Ok(())
                } else {
                    Err(format!("HTTP {}", response.status()))
                }
            }
            Err(e) => Err(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[tokio::test]
    async fn test_health_checker_creation() {
        let checks = vec![HealthCheck {
            url: "http://localhost:8080/health".to_string(),
            interval: Duration::from_secs(5),
            timeout: Duration::from_secs(2),
            alive: Arc::new(AtomicBool::new(true)),
            listener_addr: ":2055".to_string(),
            backend_addr: "10.0.0.1:2055".to_string(),
        }];

        let checker = HealthChecker::new(checks);
        assert_eq!(checker.checks.len(), 1);
    }

    #[tokio::test]
    async fn test_health_checker_empty() {
        let checker = HealthChecker::new(vec![]);
        assert!(checker.checks.is_empty());

        let (shutdown_tx, shutdown_rx) = mpsc::channel(1);

        let handle = tokio::spawn(async move {
            checker.run(shutdown_rx).await;
        });

        tokio::time::sleep(Duration::from_millis(10)).await;
        drop(shutdown_tx);

        let _ = handle.await;
    }

    #[tokio::test]
    async fn test_perform_check_invalid_url() {
        let client = Client::new();
        let result = HealthChecker::perform_check(
            &client,
            "http://invalid.invalid.invalid:12345/health",
            Duration::from_millis(100),
        )
        .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_health_check_struct() {
        let alive = Arc::new(AtomicBool::new(true));
        let check = HealthCheck {
            url: "http://localhost:8080/health".to_string(),
            interval: Duration::from_secs(5),
            timeout: Duration::from_secs(2),
            alive: Arc::clone(&alive),
            listener_addr: ":2055".to_string(),
            backend_addr: "10.0.0.1:2055".to_string(),
        };

        assert!(check.alive.load(Ordering::Relaxed));
        check.alive.store(false, Ordering::Relaxed);
        assert!(!alive.load(Ordering::Relaxed));
    }
}
