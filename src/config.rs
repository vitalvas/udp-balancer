use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("failed to read config file: {0}")]
    ReadFile(#[from] std::io::Error),
    #[error("failed to parse config: {0}")]
    Parse(#[from] serde_yaml::Error),
    #[error("validation error: {0}")]
    Validation(String),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_status_addr")]
    pub status: String,
    #[serde(default)]
    pub servers: Vec<ServerConfig>,
}

fn default_status_addr() -> String {
    ":8989".to_string()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    pub address: String,
    #[serde(default)]
    pub listener_mode: ListenerMode,
    #[serde(default)]
    pub listener_workers: u16,
    pub balancer: Option<BalancerConfig>,
    pub mirror: Option<MirrorSectionConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MirrorSectionConfig {
    #[serde(default)]
    pub targets: Vec<MirrorConfig>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Algorithm {
    #[default]
    Wrr,
    Rh,
    Maglev,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ListenerMode {
    Standard,
    #[default]
    Batch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HashKeyField {
    SrcIp,
    DstIp,
    SrcPort,
    DstPort,
    Ipfix,
    IpfixTemplateId,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BalancerConfig {
    #[serde(default)]
    pub algorithm: Algorithm,
    #[serde(default = "default_hash_key")]
    pub hash_key: Vec<HashKeyField>,
    #[serde(default = "default_proxy_timeout", with = "humantime_serde")]
    pub proxy_timeout: Duration,
    #[serde(default)]
    pub backends: Vec<BackendConfig>,
}

fn default_hash_key() -> Vec<HashKeyField> {
    vec![
        HashKeyField::SrcIp,
        HashKeyField::DstIp,
        HashKeyField::SrcPort,
        HashKeyField::DstPort,
    ]
}

fn default_proxy_timeout() -> Duration {
    Duration::from_secs(30)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackendConfig {
    pub address: String,
    #[serde(default = "default_weight")]
    pub weight: f64,
    #[serde(default)]
    pub preserve_src_address: bool,
    pub source_address: Option<String>,
    pub fallback_source_address: Option<String>,
    pub healthcheck: Option<HealthCheckConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthCheckConfig {
    pub url: String,
    #[serde(default = "default_check_interval", with = "humantime_serde")]
    pub interval: Duration,
    #[serde(default = "default_check_timeout", with = "humantime_serde")]
    pub timeout: Duration,
}

fn default_weight() -> f64 {
    1.0
}

fn default_check_interval() -> Duration {
    Duration::from_secs(5)
}

fn default_check_timeout() -> Duration {
    Duration::from_secs(2)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MirrorConfig {
    pub address: String,
    #[serde(default)]
    pub preserve_src_address: bool,
    pub source_address: Option<String>,
    pub fallback_source_address: Option<String>,
    #[serde(default = "default_probability")]
    pub probability: f64,
}

fn default_probability() -> f64 {
    1.0
}

impl Config {
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self, ConfigError> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = serde_yaml::from_str(&content)?;
        config.validate()?;
        Ok(config)
    }

    #[allow(dead_code)]
    pub fn from_str(content: &str) -> Result<Self, ConfigError> {
        let config: Config = serde_yaml::from_str(content)?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        self.validate_status_addr()?;
        for (i, server) in self.servers.iter().enumerate() {
            self.validate_server(server, i)?;
        }
        Ok(())
    }

    fn validate_status_addr(&self) -> Result<(), ConfigError> {
        parse_address(&self.status).map_err(|e| {
            ConfigError::Validation(format!("invalid status address '{}': {}", self.status, e))
        })?;
        Ok(())
    }

    fn validate_server(&self, server: &ServerConfig, index: usize) -> Result<(), ConfigError> {
        parse_address(&server.address).map_err(|e| {
            ConfigError::Validation(format!(
                "server[{}]: invalid address '{}': {}",
                index, server.address, e
            ))
        })?;

        if server.listener_workers > 1024 {
            return Err(ConfigError::Validation(format!(
                "server[{}]: listener_workers must be 0-1024, got {}",
                index, server.listener_workers
            )));
        }

        let has_mirror = server
            .mirror
            .as_ref()
            .is_some_and(|m| !m.targets.is_empty());

        if server.balancer.is_none() && !has_mirror {
            return Err(ConfigError::Validation(format!(
                "server[{}]: must have at least one balancer or mirror",
                index
            )));
        }

        if let Some(ref balancer) = server.balancer {
            self.validate_balancer(balancer, index)?;
        }

        if let Some(ref mirror_section) = server.mirror {
            for (j, mirror) in mirror_section.targets.iter().enumerate() {
                self.validate_mirror(mirror, index, j)?;
            }
        }

        Ok(())
    }

    fn validate_balancer(
        &self,
        balancer: &BalancerConfig,
        index: usize,
    ) -> Result<(), ConfigError> {
        if balancer.backends.is_empty() {
            return Err(ConfigError::Validation(format!(
                "server[{}]: balancer must have at least one backend",
                index
            )));
        }

        if (balancer.algorithm == Algorithm::Rh || balancer.algorithm == Algorithm::Maglev)
            && balancer.hash_key.is_empty()
        {
            return Err(ConfigError::Validation(format!(
                "server[{}]: {} algorithm requires at least one hash_key field",
                index,
                match balancer.algorithm {
                    Algorithm::Rh => "rh",
                    Algorithm::Maglev => "maglev",
                    _ => unreachable!(),
                }
            )));
        }

        if balancer.proxy_timeout.is_zero() {
            return Err(ConfigError::Validation(format!(
                "server[{}]: proxy_timeout must be greater than 0",
                index
            )));
        }

        for (j, backend) in balancer.backends.iter().enumerate() {
            self.validate_backend(backend, index, j)?;
        }

        Ok(())
    }

    fn validate_backend(
        &self,
        backend: &BackendConfig,
        server_index: usize,
        backend_index: usize,
    ) -> Result<(), ConfigError> {
        parse_address(&backend.address).map_err(|e| {
            ConfigError::Validation(format!(
                "server[{}].backend[{}]: invalid address '{}': {}",
                server_index, backend_index, backend.address, e
            ))
        })?;

        if backend.weight <= 0.0 {
            return Err(ConfigError::Validation(format!(
                "server[{}].backend[{}]: weight must be positive, got {}",
                server_index, backend_index, backend.weight
            )));
        }

        if backend.preserve_src_address && backend.source_address.is_some() {
            return Err(ConfigError::Validation(format!(
                "server[{}].backend[{}]: cannot use both preserve_src_address and source_address",
                server_index, backend_index
            )));
        }

        if let Some(ref fallback) = backend.fallback_source_address {
            if !backend.preserve_src_address {
                return Err(ConfigError::Validation(format!(
                    "server[{}].backend[{}]: fallback_source_address requires preserve_src_address",
                    server_index, backend_index
                )));
            }

            let fallback_ip: std::net::IpAddr = fallback.parse().map_err(|e| {
                ConfigError::Validation(format!(
                    "server[{}].backend[{}]: invalid fallback_source_address '{}': {}",
                    server_index, backend_index, fallback, e
                ))
            })?;

            if fallback_ip.is_ipv6() {
                return Err(ConfigError::Validation(format!(
                    "server[{}].backend[{}]: fallback_source_address must be IPv4",
                    server_index, backend_index
                )));
            }
        }

        if let Some(ref src_addr) = backend.source_address {
            let src_ip: std::net::IpAddr = src_addr.parse().map_err(|e| {
                ConfigError::Validation(format!(
                    "server[{}].backend[{}]: invalid source_address '{}': {}",
                    server_index, backend_index, src_addr, e
                ))
            })?;

            let backend_addr = parse_address(&backend.address).map_err(|e| {
                ConfigError::Validation(format!(
                    "server[{}].backend[{}]: invalid address '{}': {}",
                    server_index, backend_index, backend.address, e
                ))
            })?;

            // IPv4 source -> IPv6 backend is allowed (uses RFC 6052 mapping)
            // IPv6 source -> IPv4 backend is not allowed
            if src_ip.is_ipv6() && backend_addr.is_ipv4() {
                return Err(ConfigError::Validation(format!(
                    "server[{}].backend[{}]: IPv6 source_address cannot be used with IPv4 backend",
                    server_index, backend_index
                )));
            }
        }

        if let Some(ref healthcheck) = backend.healthcheck {
            if !healthcheck.url.starts_with("http://") && !healthcheck.url.starts_with("https://") {
                return Err(ConfigError::Validation(format!(
                    "server[{}].backend[{}]: healthcheck.url must start with http:// or https://",
                    server_index, backend_index
                )));
            }

            if healthcheck.interval.is_zero() {
                return Err(ConfigError::Validation(format!(
                    "server[{}].backend[{}]: healthcheck.interval must be greater than 0",
                    server_index, backend_index
                )));
            }

            if healthcheck.timeout.is_zero() {
                return Err(ConfigError::Validation(format!(
                    "server[{}].backend[{}]: healthcheck.timeout must be greater than 0",
                    server_index, backend_index
                )));
            }
        }

        Ok(())
    }

    fn validate_mirror(
        &self,
        mirror: &MirrorConfig,
        server_index: usize,
        mirror_index: usize,
    ) -> Result<(), ConfigError> {
        parse_address(&mirror.address).map_err(|e| {
            ConfigError::Validation(format!(
                "server[{}].mirror.targets[{}]: invalid address '{}': {}",
                server_index, mirror_index, mirror.address, e
            ))
        })?;

        if mirror.probability < 0.0 || mirror.probability > 1.0 {
            return Err(ConfigError::Validation(format!(
                "server[{}].mirror.targets[{}]: probability must be between 0.0 and 1.0, got {}",
                server_index, mirror_index, mirror.probability
            )));
        }

        if mirror.preserve_src_address && mirror.source_address.is_some() {
            return Err(ConfigError::Validation(format!(
                "server[{}].mirror.targets[{}]: cannot use both preserve_src_address and source_address",
                server_index, mirror_index
            )));
        }

        if let Some(ref fallback) = mirror.fallback_source_address {
            if !mirror.preserve_src_address {
                return Err(ConfigError::Validation(format!(
                    "server[{}].mirror.targets[{}]: fallback_source_address requires preserve_src_address",
                    server_index, mirror_index
                )));
            }

            let fallback_ip: std::net::IpAddr = fallback.parse().map_err(|e| {
                ConfigError::Validation(format!(
                    "server[{}].mirror.targets[{}]: invalid fallback_source_address '{}': {}",
                    server_index, mirror_index, fallback, e
                ))
            })?;

            if fallback_ip.is_ipv6() {
                return Err(ConfigError::Validation(format!(
                    "server[{}].mirror.targets[{}]: fallback_source_address must be IPv4",
                    server_index, mirror_index
                )));
            }
        }

        if let Some(ref src_addr) = mirror.source_address {
            let src_ip: std::net::IpAddr = src_addr.parse().map_err(|e| {
                ConfigError::Validation(format!(
                    "server[{}].mirror.targets[{}]: invalid source_address '{}': {}",
                    server_index, mirror_index, src_addr, e
                ))
            })?;

            let mirror_addr = parse_address(&mirror.address).map_err(|e| {
                ConfigError::Validation(format!(
                    "server[{}].mirror.targets[{}]: invalid address '{}': {}",
                    server_index, mirror_index, mirror.address, e
                ))
            })?;

            // IPv4 source -> IPv6 mirror is allowed (uses RFC 6052 mapping)
            // IPv6 source -> IPv4 mirror is not allowed
            if src_ip.is_ipv6() && mirror_addr.is_ipv4() {
                return Err(ConfigError::Validation(format!(
                    "server[{}].mirror.targets[{}]: IPv6 source_address cannot be used with IPv4 mirror",
                    server_index, mirror_index
                )));
            }
        }

        Ok(())
    }
}

fn parse_address(addr: &str) -> Result<SocketAddr, String> {
    if let Some(port_str) = addr.strip_prefix(':') {
        let port: u16 = port_str
            .parse()
            .map_err(|e| format!("invalid port: {}", e))?;
        // Use IPv6 unspecified address for dual-stack support
        Ok(SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, port)))
    } else {
        addr.parse().map_err(|e| format!("invalid address: {}", e))
    }
}

pub fn parse_socket_addr(addr: &str) -> Result<SocketAddr, String> {
    parse_address(addr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_address_with_port_only() {
        let addr = parse_address(":8080").unwrap();
        assert_eq!(addr.port(), 8080);
        assert_eq!(addr.ip(), std::net::Ipv6Addr::UNSPECIFIED);
    }

    #[test]
    fn test_parse_address_full() {
        let addr = parse_address("127.0.0.1:8080").unwrap();
        assert_eq!(addr.port(), 8080);
        assert_eq!(addr.ip(), std::net::Ipv4Addr::new(127, 0, 0, 1));
    }

    #[test]
    fn test_parse_address_invalid() {
        assert!(parse_address("invalid").is_err());
        assert!(parse_address(":invalid").is_err());
    }

    #[test]
    fn test_config_minimal_balancer() {
        let yaml = r#"
servers:
  - address: ":2055"
    balancer:
      backends:
        - address: "10.0.0.1:2055"
"#;
        let config = Config::from_str(yaml).unwrap();
        assert_eq!(config.servers.len(), 1);
        assert_eq!(config.status, ":8989");
        let balancer = config.servers[0].balancer.as_ref().unwrap();
        assert_eq!(balancer.algorithm, Algorithm::Wrr);
        assert_eq!(balancer.backends.len(), 1);
        assert_eq!(balancer.backends[0].weight, 1.0);
    }

    #[test]
    fn test_config_rh_algorithm() {
        let yaml = r#"
servers:
  - address: ":2055"
    balancer:
      algorithm: rh
      hash_key:
        - src_ip
        - dst_ip
      backends:
        - address: "10.0.0.1:2055"
          weight: 2.0
"#;
        let config = Config::from_str(yaml).unwrap();
        let balancer = config.servers[0].balancer.as_ref().unwrap();
        assert_eq!(balancer.algorithm, Algorithm::Rh);
        assert_eq!(balancer.hash_key.len(), 2);
        assert_eq!(balancer.hash_key[0], HashKeyField::SrcIp);
        assert_eq!(balancer.hash_key[1], HashKeyField::DstIp);
    }

    #[test]
    fn test_config_mirror_only() {
        let yaml = r#"
servers:
  - address: ":514"
    mirror:
      targets:
        - address: "10.0.0.1:514"
        - address: "10.0.0.2:514"
          probability: 0.5
"#;
        let config = Config::from_str(yaml).unwrap();
        assert!(config.servers[0].balancer.is_none());
        let mirror = config.servers[0].mirror.as_ref().unwrap();
        assert_eq!(mirror.targets.len(), 2);
        assert_eq!(mirror.targets[0].probability, 1.0);
        assert_eq!(mirror.targets[1].probability, 0.5);
    }

    #[test]
    fn test_config_with_health_check() {
        let yaml = r#"
servers:
  - address: ":2055"
    balancer:
      backends:
        - address: "10.0.0.1:2055"
          healthcheck:
            url: "http://10.0.0.1:8080/health"
            interval: 10s
            timeout: 3s
"#;
        let config = Config::from_str(yaml).unwrap();
        let backend = &config.servers[0].balancer.as_ref().unwrap().backends[0];
        let healthcheck = backend.healthcheck.as_ref().unwrap();
        assert_eq!(healthcheck.url, "http://10.0.0.1:8080/health");
        assert_eq!(healthcheck.interval, Duration::from_secs(10));
        assert_eq!(healthcheck.timeout, Duration::from_secs(3));
    }

    #[test]
    fn test_config_preserve_src_address() {
        let yaml = r#"
servers:
  - address: ":50000"
    mirror:
      targets:
        - address: "127.0.0.1:50001"
          preserve_src_address: true
"#;
        let config = Config::from_str(yaml).unwrap();
        let mirror = config.servers[0].mirror.as_ref().unwrap();
        assert!(mirror.targets[0].preserve_src_address);
    }

    #[test]
    fn test_config_source_address() {
        let yaml = r#"
servers:
  - address: ":50000"
    mirror:
      targets:
        - address: "10.0.0.100:50000"
          source_address: "192.0.2.1"
"#;
        let config = Config::from_str(yaml).unwrap();
        let mirror = config.servers[0].mirror.as_ref().unwrap();
        assert_eq!(
            mirror.targets[0].source_address.as_ref().unwrap(),
            "192.0.2.1"
        );
    }

    #[test]
    fn test_validation_no_balancer_or_mirror() {
        let yaml = r#"
servers:
  - address: ":2055"
"#;
        let result = Config::from_str(yaml);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("must have at least one balancer or mirror"));
    }

    #[test]
    fn test_validation_empty_backends() {
        let yaml = r#"
servers:
  - address: ":2055"
    balancer:
      backends: []
"#;
        let result = Config::from_str(yaml);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("must have at least one backend"));
    }

    #[test]
    fn test_validation_invalid_weight() {
        let yaml = r#"
servers:
  - address: ":2055"
    balancer:
      backends:
        - address: "10.0.0.1:2055"
          weight: -1.0
"#;
        let result = Config::from_str(yaml);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("weight must be positive"));
    }

    #[test]
    fn test_validation_invalid_probability() {
        let yaml = r#"
servers:
  - address: ":514"
    mirror:
      targets:
        - address: "10.0.0.1:514"
          probability: 1.5
"#;
        let result = Config::from_str(yaml);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("probability must be between 0.0 and 1.0"));
    }

    #[test]
    fn test_validation_both_preserve_and_fake() {
        let yaml = r#"
servers:
  - address: ":514"
    mirror:
      targets:
        - address: "10.0.0.1:514"
          preserve_src_address: true
          source_address: "192.0.2.1"
"#;
        let result = Config::from_str(yaml);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("cannot use both preserve_src_address and source_address"));
    }

    #[test]
    fn test_validation_ipv6_source_to_ipv4_backend() {
        // IPv6 source -> IPv4 backend is not allowed
        let yaml = r#"
servers:
  - address: ":2055"
    balancer:
      backends:
        - address: "10.0.0.1:2055"
          source_address: "::1"
"#;
        let result = Config::from_str(yaml);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("IPv6 source_address cannot be used with IPv4 backend"));
    }

    #[test]
    fn test_validation_ipv4_source_to_ipv6_backend() {
        // IPv4 source -> IPv6 backend is allowed (RFC 6052 mapping)
        let yaml = r#"
servers:
  - address: ":2055"
    balancer:
      backends:
        - address: "[::1]:2055"
          source_address: "192.0.2.1"
"#;
        let result = Config::from_str(yaml);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validation_ipv6_source_to_ipv4_mirror() {
        // IPv6 source -> IPv4 mirror is not allowed
        let yaml = r#"
servers:
  - address: ":514"
    mirror:
      targets:
        - address: "10.0.0.1:514"
          source_address: "::1"
"#;
        let result = Config::from_str(yaml);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("IPv6 source_address cannot be used with IPv4 mirror"));
    }

    #[test]
    fn test_validation_ipv4_source_to_ipv6_mirror() {
        // IPv4 source -> IPv6 mirror is allowed (RFC 6052 mapping)
        let yaml = r#"
servers:
  - address: ":514"
    mirror:
      targets:
        - address: "[::1]:514"
          source_address: "192.0.2.1"
"#;
        let result = Config::from_str(yaml);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validation_invalid_healthcheck_url() {
        let yaml = r#"
servers:
  - address: ":2055"
    balancer:
      backends:
        - address: "10.0.0.1:2055"
          healthcheck:
            url: "ftp://invalid"
"#;
        let result = Config::from_str(yaml);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("healthcheck.url must start with http://"));
    }

    #[test]
    fn test_validation_rh_empty_hash_key() {
        let yaml = r#"
servers:
  - address: ":2055"
    balancer:
      algorithm: rh
      hash_key: []
      backends:
        - address: "10.0.0.1:2055"
"#;
        let result = Config::from_str(yaml);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("rh algorithm requires at least one hash_key"));
    }

    #[test]
    fn test_validation_maglev_empty_hash_key() {
        let yaml = r#"
servers:
  - address: ":2055"
    balancer:
      algorithm: maglev
      hash_key: []
      backends:
        - address: "10.0.0.1:2055"
"#;
        let result = Config::from_str(yaml);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("maglev algorithm requires at least one hash_key"));
    }

    #[test]
    fn test_config_maglev_algorithm() {
        let yaml = r#"
servers:
  - address: ":2055"
    balancer:
      algorithm: maglev
      hash_key:
        - src_ip
      backends:
        - address: "10.0.0.1:2055"
          weight: 1.0
"#;
        let config = Config::from_str(yaml).unwrap();
        let balancer = config.servers[0].balancer.as_ref().unwrap();
        assert_eq!(balancer.algorithm, Algorithm::Maglev);
        assert_eq!(balancer.hash_key.len(), 1);
        assert_eq!(balancer.hash_key[0], HashKeyField::SrcIp);
    }

    #[test]
    fn test_validation_zero_proxy_timeout() {
        let yaml = r#"
servers:
  - address: ":2055"
    balancer:
      proxy_timeout: 0s
      backends:
        - address: "10.0.0.1:2055"
"#;
        let result = Config::from_str(yaml);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("proxy_timeout must be greater than 0"));
    }

    #[test]
    fn test_validation_listener_workers_range() {
        let yaml = r#"
servers:
  - address: ":2055"
    listener_workers: 2000
    balancer:
      backends:
        - address: "10.0.0.1:2055"
"#;
        let result = Config::from_str(yaml);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("listener_workers must be 0-1024"));
    }

    #[test]
    fn test_default_hash_key_for_rh() {
        let yaml = r#"
servers:
  - address: ":2055"
    balancer:
      algorithm: rh
      backends:
        - address: "10.0.0.1:2055"
"#;
        let config = Config::from_str(yaml).unwrap();
        let balancer = config.servers[0].balancer.as_ref().unwrap();
        assert_eq!(balancer.hash_key.len(), 4);
    }

    #[test]
    fn test_proxy_timeout_parsing() {
        let yaml = r#"
servers:
  - address: ":2055"
    balancer:
      proxy_timeout: 60s
      backends:
        - address: "10.0.0.1:2055"
"#;
        let config = Config::from_str(yaml).unwrap();
        let balancer = config.servers[0].balancer.as_ref().unwrap();
        assert_eq!(balancer.proxy_timeout, Duration::from_secs(60));
    }
}
