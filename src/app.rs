use std::net::IpAddr;
use std::sync::Arc;

use tokio::sync::mpsc;
use tracing::{error, info};

use crate::balancer::{Balancer, Mirror, SendMode, UdpBackend};
use crate::config::{parse_socket_addr, Config, ListenerMode, MirrorConfig};
use crate::listener::{BatchListener, StandardListener};
use crate::metrics::init_metrics;
use crate::server::{HealthCheck, HealthChecker, ServerInfo, StatusServer};

pub struct App {
    config: Config,
}

impl App {
    pub fn new(config: Config) -> Self {
        Self { config }
    }

    pub async fn run(self) -> Result<(), Box<dyn std::error::Error>> {
        init_metrics();

        let status_addr = parse_socket_addr(&self.config.status)?;

        let mut listeners: Vec<mpsc::Sender<()>> = Vec::new();
        let mut session_cleanups: Vec<mpsc::Sender<()>> = Vec::new();
        let mut server_infos: Vec<ServerInfo> = Vec::new();
        let mut health_checks: Vec<HealthCheck> = Vec::new();

        for server_config in &self.config.servers {
            let server_addr = parse_socket_addr(&server_config.address)?;
            let listener_addr_str = server_config.address.clone();

            let balancer = if let Some(ref bal_cfg) = server_config.balancer {
                let mut backends = Vec::new();

                for backend_cfg in &bal_cfg.backends {
                    let backend_addr = parse_socket_addr(&backend_cfg.address)?;

                    let mode = if backend_cfg.preserve_src_address {
                        let fallback = backend_cfg
                            .fallback_source_address
                            .as_ref()
                            .map(|s| s.parse::<std::net::Ipv4Addr>())
                            .transpose()?;
                        SendMode::PreserveSrcAddr {
                            fallback_ipv4: fallback,
                        }
                    } else if let Some(ref src_addr) = backend_cfg.source_address {
                        let ip: IpAddr = src_addr.parse()?;
                        SendMode::FakeSrcAddr(ip)
                    } else {
                        SendMode::Standard
                    };

                    let backend =
                        UdpBackend::new(backend_addr, mode, 1.0, backend_cfg.weight).await?;
                    let backend = Arc::new(backend);

                    if let Some(ref healthcheck) = backend_cfg.healthcheck {
                        health_checks.push(HealthCheck {
                            url: healthcheck.url.clone(),
                            interval: healthcheck.interval,
                            timeout: healthcheck.timeout,
                            alive: backend.alive_flag(),
                            listener_addr: listener_addr_str.clone(),
                            backend_addr: backend_cfg.address.clone(),
                        });
                    }

                    backends.push(backend);
                }

                let balancer = Arc::new(Balancer::new(
                    bal_cfg.algorithm,
                    backends,
                    bal_cfg.hash_key.clone(),
                    listener_addr_str.clone(),
                    bal_cfg.proxy_timeout,
                ));
                // Start session cleanup task
                session_cleanups.push(balancer.start_session_cleanup());
                Some(balancer)
            } else {
                None
            };

            let mirror = if let Some(ref mirror_cfg) = server_config.mirror {
                if !mirror_cfg.targets.is_empty() {
                    let backends = self.create_mirror_backends(&mirror_cfg.targets).await?;
                    Some(Arc::new(Mirror::new(backends, listener_addr_str.clone())))
                } else {
                    None
                }
            } else {
                None
            };

            server_infos.push(ServerInfo {
                balancer: balancer.clone(),
                mirror: mirror.clone(),
            });

            let (shutdown_tx, shutdown_rx) = mpsc::channel(1);

            match server_config.listener_mode {
                ListenerMode::Batch => {
                    let listener = Arc::new(
                        BatchListener::new(
                            server_addr,
                            server_config.listener_workers as usize,
                            balancer.clone(),
                            mirror.clone(),
                        )
                        .await?,
                    );

                    let bound_addr = listener.bound_addr().to_string();
                    if let Some(ref bal) = balancer {
                        bal.set_listener_addr(bound_addr.clone());
                    }
                    if let Some(ref mir) = mirror {
                        mir.set_listener_addr(bound_addr);
                    }

                    tokio::spawn(async move {
                        listener.run(shutdown_rx).await;
                    });
                }
                ListenerMode::Standard => {
                    let listener = Arc::new(
                        StandardListener::new(server_addr, balancer.clone(), mirror.clone())
                            .await?,
                    );

                    let bound_addr = listener.bound_addr().to_string();
                    if let Some(ref bal) = balancer {
                        bal.set_listener_addr(bound_addr.clone());
                    }
                    if let Some(ref mir) = mirror {
                        mir.set_listener_addr(bound_addr);
                    }

                    tokio::spawn(async move {
                        listener.run(shutdown_rx).await;
                    });
                }
            }

            listeners.push(shutdown_tx);
        }

        let (checker_shutdown_tx, checker_shutdown_rx) = mpsc::channel(1);
        let health_checker = HealthChecker::new(health_checks);
        tokio::spawn(async move {
            health_checker.run(checker_shutdown_rx).await;
        });

        let (status_shutdown_tx, status_shutdown_rx) = mpsc::channel(1);
        let status_server = StatusServer::new(status_addr, server_infos);
        let status_handle = tokio::spawn(async move {
            if let Err(e) = status_server.run(status_shutdown_rx).await {
                error!("Status server error: {}", e);
            }
        });

        info!("UDP Balancer started");

        tokio::signal::ctrl_c().await?;
        info!("Received shutdown signal");

        let _ = status_shutdown_tx.send(()).await;
        let _ = checker_shutdown_tx.send(()).await;

        for shutdown_tx in listeners {
            let _ = shutdown_tx.send(()).await;
        }

        for shutdown_tx in session_cleanups {
            let _ = shutdown_tx.send(()).await;
        }

        let _ = status_handle.await;

        info!("Shutdown complete");
        Ok(())
    }

    async fn create_mirror_backends(
        &self,
        configs: &[MirrorConfig],
    ) -> Result<Vec<Arc<UdpBackend>>, Box<dyn std::error::Error>> {
        let mut backends = Vec::new();

        for cfg in configs {
            let addr = parse_socket_addr(&cfg.address)?;

            let backend = if cfg.preserve_src_address {
                let fallback = cfg
                    .fallback_source_address
                    .as_ref()
                    .map(|s| s.parse::<std::net::Ipv4Addr>())
                    .transpose()?;
                let mode = SendMode::PreserveSrcAddr {
                    fallback_ipv4: fallback,
                };
                UdpBackend::new(addr, mode, cfg.probability, 1.0).await?
            } else if let Some(ref src_addr) = cfg.source_address {
                let ip: IpAddr = src_addr.parse()?;
                let mode = SendMode::FakeSrcAddr(ip);
                UdpBackend::new(addr, mode, cfg.probability, 1.0).await?
            } else {
                UdpBackend::new_unconnected(addr, cfg.probability, 1.0).await?
            };

            backends.push(Arc::new(backend));
        }

        Ok(backends)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_app_creation() {
        let config = Config::from_str(
            r#"
servers:
  - address: ":2055"
    balancer:
      backends:
        - address: "127.0.0.1:2055"
"#,
        )
        .unwrap();

        let app = App::new(config);
        assert_eq!(app.config.servers.len(), 1);
    }
}
