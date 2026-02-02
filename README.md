# UDP Balancer

A high-performance UDP load balancer written in Rust with support for flow-consistent hashing, traffic mirroring, and health checking.

## Features

- **Load Balancing Algorithms**
  - Weighted Round Robin (WRR)
  - Rendezvous Hashing (RH)
  - Maglev Hashing

- **Configurable Hash Keys** - customize flow affinity using any combination of:
  - Source/destination IP addresses
  - Source/destination ports
  - IPFIX Observation Domain ID (for NetFlow/IPFIX collectors)

- **Traffic Mirroring** - copy traffic to multiple destinations with options to:
  - Preserve original source address (requires CAP_NET_RAW)
  - Use a fake source address
  - Probability-based sampling

- **Health Checking** - HTTP-based backend health checks with configurable intervals and timeouts
  - TLS certificate verification is disabled (accepts self-signed certificates)
  - HTTP status codes 200-499 are considered healthy (5xx are unhealthy)

- **High Performance**
  - Async I/O with Tokio runtime
  - Multi-worker listeners using SO_REUSEPORT
  - Dual-stack IPv4/IPv6 support

- **Observability**
  - Prometheus metrics endpoint (`/metrics`)
  - JSON status API (`/api/v1/status`)
  - Web dashboard (`/`)

## Usage

```bash
udp-balancer -c config.yml
```

## Configuration

See [config-example.yml](config-example.yml) for all available options.

### Minimal example

```yaml
servers:
  - address: ":1053"
    balancer:
      algorithm: rh
      backends:
        - address: "8.8.8.8:53"
        - address: "8.8.4.4:53"
```

### With Maglev hashing

```yaml
servers:
  - address: ":2055"
    balancer:
      algorithm: maglev
      hash_key:
        - src_ip
      backends:
        - address: "10.0.0.1:2055"
        - address: "10.0.0.2:2055"
        - address: "10.0.0.3:2055"
```

### With mirroring

```yaml
servers:
  - address: ":514"
    balancer:
      algorithm: wrr
      backends:
        - address: "10.0.0.1:514"
          weight: 2.0
        - address: "10.0.0.2:514"
          weight: 1.0
    mirror:
      targets:
        - address: "10.0.0.100:514"
          probability: 0.1  # Sample 10% of traffic
```

### Configuration options

| Option | Description | Default |
|--------|-------------|---------|
| `status` | HTTP status server address | `:8989` |
| `servers[].address` | Listener address (`:port` for dual-stack) | required |
| `servers[].listener_mode` | `standard` or `batch` (multi-worker with SO_REUSEPORT) | `batch` |
| `servers[].listener_workers` | Number of workers for batch mode (0 = auto) | `0` |
| `servers[].balancer.algorithm` | `wrr`, `rh`, or `maglev` | `wrr` |
| `servers[].balancer.hash_key` | Hash key fields (see Hash Keys section) | all 4 |
| `servers[].balancer.proxy_timeout` | Session timeout for responses | `30s` |
| `servers[].balancer.backends[].address` | Backend address | required |
| `servers[].balancer.backends[].weight` | Backend weight | `1.0` |
| `servers[].balancer.backends[].preserve_src_address` | Keep original source IP | `false` |
| `servers[].balancer.backends[].source_address` | Use custom source IP | none |
| `servers[].balancer.backends[].fallback_source_address` | IPv4 fallback for IPv6 clients (with preserve_src_address) | none |
| `servers[].balancer.backends[].healthcheck.url` | Health check URL | none |
| `servers[].balancer.backends[].healthcheck.interval` | Health check interval | `5s` |
| `servers[].balancer.backends[].healthcheck.timeout` | Health check timeout | `2s` |
| `servers[].mirror.targets[].address` | Mirror destination | required |
| `servers[].mirror.targets[].probability` | Send probability (0.0-1.0) | `1.0` |
| `servers[].mirror.targets[].preserve_src_address` | Keep original source IP | `false` |
| `servers[].mirror.targets[].source_address` | Use custom source IP | none |
| `servers[].mirror.targets[].fallback_source_address` | IPv4 fallback for IPv6 clients (with preserve_src_address) | none |

## Source Address Handling

When using `preserve_src_address` or `source_address`, the balancer uses raw sockets to send packets with custom source addresses.

### Dual-Stack Support

Listeners bound to `:port` accept both IPv4 and IPv6 clients. Address translation is handled automatically:

| Client | Backend | Behavior |
|--------|---------|----------|
| IPv4 | IPv4 | Direct |
| IPv4 | IPv6 | RFC 6052 mapping (`64:ff9b::/96` prefix) |
| IPv6 | IPv6 | Direct |
| IPv6 | IPv4 | Requires `fallback_source_address` |

### Example with fallback

```yaml
servers:
  - address: ":514"  # Dual-stack listener
    balancer:
      backends:
        - address: "10.0.0.1:514"  # IPv4 backend
          preserve_src_address: true
          fallback_source_address: "192.0.2.100"  # Used for IPv6 clients
```

## Health Checking

Backends can be monitored using HTTP health checks. When a backend becomes unhealthy, traffic is automatically redistributed to healthy backends.

### Behavior

- **TLS verification disabled** - accepts self-signed and invalid certificates
- **Healthy status codes** - HTTP 200-499 (any response except server errors)
- **Unhealthy** - HTTP 5xx, connection refused, timeout, or other network errors

### Example

```yaml
servers:
  - address: ":1053"
    balancer:
      backends:
        - address: "8.8.8.8:53"
          healthcheck:
            url: "https://8.8.8.8/"
            interval: 10s
            timeout: 5s
        - address: "1.1.1.1:53"
          healthcheck:
            url: "https://1.1.1.1/"
```

## HTTP Endpoints

| Endpoint | Description |
|----------|-------------|
| `GET /` | Web dashboard |
| `GET /ping` | Health check (returns `{"status":"ok"}`) |
| `GET /api/v1/status` | JSON status with stats |
| `GET /metrics` | Prometheus metrics |

## Metrics

All metrics are prefixed with `udp_balancer_`.

- `udp_balancer_packets_received_total` - Packets received by listener
- `udp_balancer_bytes_received_total` - Bytes received by listener
- `udp_balancer_packets_sent_total` - Response packets sent to clients
- `udp_balancer_bytes_sent_total` - Response bytes sent to clients
- `udp_balancer_backend_packets_sent_total` - Packets forwarded to backends
- `udp_balancer_backend_bytes_sent_total` - Bytes forwarded to backends
- `udp_balancer_backend_packets_failed_total` - Failed forwards to backends
- `udp_balancer_backend_responses_received_total` - Responses from backends
- `udp_balancer_backend_response_bytes_received_total` - Response bytes from backends
- `udp_balancer_backend_alive` - Backend health status (1=up, 0=down)
- `udp_balancer_mirror_packets_sent_total` - Packets sent to mirrors
- `udp_balancer_mirror_bytes_sent_total` - Bytes sent to mirrors
- `udp_balancer_mirror_packets_failed_total` - Failed sends to mirrors
- `udp_balancer_health_check_total` - Health checks performed
- `udp_balancer_health_check_failed_total` - Failed health checks

## Load Balancing Algorithms

| Algorithm | Lookup Time | Flow Affinity | Best For |
|-----------|-------------|---------------|----------|
| WRR | O(1) | No | Stateless protocols, even distribution |
| RH | O(n) | Yes | Small backend pools, weighted distribution |
| Maglev | O(1) | Yes | Large backend pools, high throughput |

### Weighted Round Robin (WRR)

Distributes packets across backends in rotation, respecting weights. Best for stateless protocols where flow affinity is not required.

### Rendezvous Hashing (RH)

Uses consistent hashing to ensure the same client always reaches the same backend. Lookup time is O(n) where n is the number of backends. When a backend fails, only its traffic is redistributed.

### Maglev Hashing

Google's consistent hashing algorithm using a precomputed lookup table. Provides O(1) lookup time regardless of backend count. The table size is 65537 (prime number) for optimal distribution. When a backend fails, only its traffic is redistributed with minimal disruption to other flows.

## Hash Keys

Hash keys determine how flows are mapped to backends when using RH or Maglev algorithms. The hash is computed from the selected fields.

| Field | Description |
|-------|-------------|
| `src_ip` | Source IP address |
| `dst_ip` | Destination IP address |
| `src_port` | Source UDP port |
| `dst_port` | Destination UDP port |
| `ipfix` | IPFIX Observation Domain ID (bytes 12-15 of header) |
| `ipfix_template_id` | IPFIX Template ID from first Data Set (bytes 16-17) |

Default: `[src_ip, dst_ip, src_port, dst_port]` (5-tuple without protocol)

### Common configurations

| Use Case | Hash Keys | Reason |
|----------|-----------|--------|
| Standard 5-tuple | `[src_ip, dst_ip, src_port, dst_port]` | Full flow identification |
| Source IP only | `[src_ip]` | All traffic from same client to same backend |
| TFTP | `[src_ip]` | Client changes ports during transfer |
| RADIUS | `[src_ip, dst_ip, dst_port]` | Ignore ephemeral source port |
| NetFlow/IPFIX | `[src_ip]` or `[ipfix]` | Per-exporter or per-observation-domain |
| Multi-tenant IPFIX | `[src_ip, ipfix]` | Combine exporter and domain ID |
| IPFIX by template | `[ipfix_template_id]` | Route by data type (template) |
| IPFIX full | `[src_ip, ipfix, ipfix_template_id]` | Exporter + domain + template |

### Example

```yaml
servers:
  # IPFIX with observation domain ID hashing
  - address: ":2055"
    balancer:
      algorithm: maglev
      hash_key:
        - ipfix
      backends:
        - address: "10.0.0.1:2055"
        - address: "10.0.0.2:2055"

  # SNMP traps - hash by source IP only
  - address: ":162"
    balancer:
      algorithm: rh
      hash_key:
        - src_ip
      backends:
        - address: "10.0.0.1:162"
        - address: "10.0.0.2:162"
```

## Use Cases

- NetFlow/IPFIX/sFlow collectors
- Syslog aggregation
- DNS load balancing
- SNMP trap receivers
- Streaming telemetry
- Any UDP-based protocol requiring load distribution or mirroring

## Requirements

- Linux (uses SO_REUSEPORT, raw sockets)
- Root privileges or CAP_NET_RAW capability for source address preservation

## License

MIT
