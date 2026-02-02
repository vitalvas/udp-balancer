use std::net::SocketAddr;
use std::sync::Arc;

use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{Html, Response},
    routing::get,
    Json, Router,
};
use serde::Serialize;
use tokio::sync::mpsc;
use tracing::info;

use crate::balancer::{Balancer, Mirror};
use crate::config::{Algorithm, HashKeyField};
use crate::metrics::{gather_metrics, get_backend_stats, get_listener_stats, get_mirror_stats};

const VERSION: &str = env!("CARGO_PKG_VERSION");

const DASHBOARD_HTML: &str = r#"<!DOCTYPE html>
<html>
<head>
    <meta charset="utf-8">
    <title>UDP Balancer</title>
    <style>
        *{box-sizing:border-box;margin:0;padding:0}
        body{font-family:monospace;font-size:12px;background:#f5f5f5;color:#333;padding:10px}
        .hdr{display:flex;justify-content:space-between;align-items:center;margin-bottom:10px;padding-bottom:6px;border-bottom:1px solid #999}
        .hdr h1{font-size:14px}
        .hdr .ver{color:#666;font-size:11px}
        .ctrl{display:flex;align-items:center;gap:10px}
        btn,button{font-family:monospace;font-size:11px;padding:2px 8px;cursor:pointer;background:#ddd;border:1px solid #999}
        .upd{font-size:10px;color:#888}
        .err{background:#fcc;padding:6px;margin-bottom:8px;border:1px solid #c00;font-size:11px}
        .sec{background:#fff;border:1px solid #ccc;margin-bottom:10px}
        .sec-h{background:#e0e0e0;padding:6px 8px;font-weight:bold;font-size:11px;border-bottom:1px solid #ccc}
        .sec-c{padding:8px}
        table{width:100%;border-collapse:collapse;font-size:11px}
        th,td{text-align:left;padding:4px 6px;border:1px solid #ddd}
        th{background:#f0f0f0}
        tbody tr:nth-child(odd){background:#f9f9f9}
        tbody tr:hover{background:#e8f4fc}
        .r{text-align:right}
        .ok{background:#cfc}
        .fail{background:#fcc}
        .muted{color:#888}
        .tag{display:inline-block;padding:1px 4px;background:#e0e0e0;border-radius:2px;font-size:10px;margin-left:6px}
        .subsec{margin-top:8px}
        .subsec-h{font-weight:bold;font-size:10px;color:#666;margin-bottom:4px}
    </style>
</head>
<body>
    <div class="hdr">
        <h1>UDP Balancer <span class="ver">v<span id="ver"></span></span></h1>
        <div class="ctrl">
            <button onclick="toggle()">Pause</button>
            <span class="upd"><span id="upd">-</span></span>
        </div>
    </div>
    <div id="err"></div>
    <div id="out"></div>
    <script>
        let p=false;
        function toggle(){p=!p;document.querySelector('button').textContent=p?'Resume':'Pause';if(!p)load()}
        function fmt(n){if(n>=1e9)return(n/1e9).toFixed(1)+'G';if(n>=1e6)return(n/1e6).toFixed(1)+'M';if(n>=1e3)return(n/1e3).toFixed(1)+'K';return n}
        async function load(){
            if(p)return;
            try{
                const r=await fetch('/api/v1/status');
                if(!r.ok)throw new Error('HTTP '+r.status);
                const d=await r.json();
                render(d);
                document.getElementById('upd').textContent=new Date().toLocaleTimeString();
                document.getElementById('err').innerHTML='';
            }catch(e){
                document.getElementById('err').innerHTML='<div class="err">'+e.message+'</div>';
            }
        }
        function render(d){
            document.getElementById('ver').textContent=d.version;
            let h='';
            if(d.servers.length===0){
                h+='<div class="sec"><div class="sec-h">Listeners</div><div class="sec-c muted">None</div></div>';
            }else{
                d.servers.forEach(s=>{
                    let algoTag=s.algorithm?'<span class="tag">'+s.algorithm.toUpperCase()+(s.hash_key&&s.hash_key.length?'<span style="font-weight:normal">('+s.hash_key.join(', ')+')</span>':'')+'</span>':'';
                    h+='<div class="sec"><div class="sec-h">'+esc(s.listener)+algoTag+' <span style="font-weight:normal;color:#666">[RX: '+fmt(s.stats.rx_packets)+' pkts / '+fmt(s.stats.rx_bytes)+' B | TX: '+fmt(s.stats.tx_packets)+' pkts / '+fmt(s.stats.tx_bytes)+' B]</span></div>';
                    h+='<div class="sec-c">';
                    if(s.backends && s.backends.length>0){
                        h+='<div class="subsec"><div class="subsec-h">Backends</div>';
                        h+='<table><thead><tr><th>Address</th><th class="r">Wt</th><th class="r">TX pkts</th><th class="r">TX bytes</th><th class="r">RX pkts</th><th class="r">RX bytes</th><th class="r">Failed</th><th>Status</th></tr></thead><tbody>';
                        s.backends.forEach(b=>{
                            const cls=b.alive?'ok':'fail',lbl=b.alive?'UP':'DOWN';
                            h+='<tr><td>'+esc(b.address)+'</td><td class="r">'+b.weight.toFixed(1)+'</td>';
                            h+='<td class="r">'+fmt(b.stats.tx_packets)+'</td><td class="r">'+fmt(b.stats.tx_bytes)+'</td>';
                            h+='<td class="r">'+fmt(b.stats.rx_packets)+'</td><td class="r">'+fmt(b.stats.rx_bytes)+'</td>';
                            h+='<td class="r">'+(b.stats.tx_failed>0?'<b style="color:#c00">'+fmt(b.stats.tx_failed)+'</b>':'0')+'</td>';
                            h+='<td class="'+cls+'">'+lbl+'</td></tr>';
                        });
                        h+='</tbody></table></div>';
                    }
                    if(s.mirrors && s.mirrors.length>0){
                        h+='<div class="subsec"><div class="subsec-h">Mirrors</div>';
                        h+='<table><thead><tr><th>Address</th><th class="r">Prob</th><th class="r">TX pkts</th><th class="r">TX bytes</th><th class="r">Failed</th></tr></thead><tbody>';
                        s.mirrors.forEach(m=>{
                            h+='<tr><td>'+esc(m.address)+'</td><td class="r">'+(m.probability*100).toFixed(0)+'%</td>';
                            h+='<td class="r">'+fmt(m.stats.tx_packets)+'</td><td class="r">'+fmt(m.stats.tx_bytes)+'</td>';
                            h+='<td class="r">'+(m.stats.tx_failed>0?'<b style="color:#c00">'+fmt(m.stats.tx_failed)+'</b>':'0')+'</td></tr>';
                        });
                        h+='</tbody></table></div>';
                    }
                    if((!s.backends||s.backends.length===0)&&(!s.mirrors||s.mirrors.length===0)){
                        h+='<span class="muted">No backends or mirrors</span>';
                    }
                    h+='</div></div>';
                });
            }
            document.getElementById('out').innerHTML=h;
        }
        function esc(t){const d=document.createElement('div');d.textContent=t;return d.innerHTML}
        load();setInterval(load,5000);
    </script>
</body>
</html>
"#;

#[derive(Clone)]
pub struct ServerInfo {
    pub balancer: Option<Arc<Balancer>>,
    pub mirror: Option<Arc<Mirror>>,
}

#[derive(Clone)]
pub struct AppState {
    pub servers: Vec<ServerInfo>,
}

#[derive(Serialize)]
pub struct PingResponse {
    pub status: String,
}

#[derive(Serialize)]
pub struct ListenerStats {
    pub rx_packets: u64,
    pub rx_bytes: u64,
    pub tx_packets: u64,
    pub tx_bytes: u64,
}

#[derive(Serialize)]
pub struct BackendStats {
    pub rx_packets: u64,
    pub rx_bytes: u64,
    pub tx_packets: u64,
    pub tx_bytes: u64,
    pub tx_failed: u64,
}

#[derive(Serialize)]
pub struct MirrorStats {
    pub tx_packets: u64,
    pub tx_bytes: u64,
    pub tx_failed: u64,
}

#[derive(Serialize)]
pub struct BackendStatus {
    pub address: String,
    pub alive: bool,
    pub weight: f64,
    pub stats: BackendStats,
}

#[derive(Serialize)]
pub struct MirrorStatus {
    pub address: String,
    pub probability: f64,
    pub stats: MirrorStats,
}

#[derive(Serialize)]
pub struct ServerStatus {
    pub listener: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub algorithm: Option<Algorithm>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hash_key: Vec<HashKeyField>,
    pub stats: ListenerStats,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub backends: Vec<BackendStatus>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub mirrors: Vec<MirrorStatus>,
}

#[derive(Serialize)]
pub struct StatusResponse {
    pub version: String,
    pub servers: Vec<ServerStatus>,
}

pub struct StatusServer {
    address: SocketAddr,
    state: AppState,
}

impl StatusServer {
    pub fn new(address: SocketAddr, servers: Vec<ServerInfo>) -> Self {
        Self {
            address,
            state: AppState { servers },
        }
    }

    pub async fn run(self, mut shutdown_rx: mpsc::Receiver<()>) -> Result<(), std::io::Error> {
        let app = Router::new()
            .route("/", get(dashboard_handler))
            .route("/ping", get(ping_handler))
            .route("/api/v1/status", get(api_status_handler))
            .route("/metrics", get(metrics_handler))
            .with_state(self.state);

        let listener = tokio::net::TcpListener::bind(self.address).await?;
        let local_addr = listener.local_addr()?;

        info!("Status server listening on http://{}", local_addr);

        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                shutdown_rx.recv().await;
                info!("Status server shutting down");
            })
            .await?;

        Ok(())
    }
}

async fn dashboard_handler() -> Html<&'static str> {
    Html(DASHBOARD_HTML)
}

async fn ping_handler() -> Json<PingResponse> {
    Json(PingResponse {
        status: "ok".to_string(),
    })
}

async fn api_status_handler(State(state): State<AppState>) -> Json<StatusResponse> {
    let mut servers = Vec::new();

    for server_info in &state.servers {
        let listener_addr = if let Some(ref balancer) = server_info.balancer {
            balancer.listener_addr()
        } else if let Some(ref mirror) = server_info.mirror {
            mirror.listener_addr()
        } else {
            continue;
        };

        let (rx_packets, rx_bytes, tx_packets, tx_bytes) = get_listener_stats(&listener_addr);

        let mut backends = Vec::new();
        let mut algorithm = None;
        let mut hash_key = Vec::new();

        if let Some(ref balancer) = server_info.balancer {
            algorithm = Some(balancer.algorithm_type());
            if balancer.algorithm_type() != Algorithm::Wrr {
                hash_key = balancer.hash_key_fields().to_vec();
            }

            for backend in balancer.backends() {
                let backend_addr = backend.address().to_string();
                let stats = get_backend_stats(&listener_addr, &backend_addr);

                backends.push(BackendStatus {
                    address: backend_addr,
                    alive: backend.is_alive(),
                    weight: backend.weight(),
                    stats: BackendStats {
                        rx_packets: stats.rx_packets,
                        rx_bytes: stats.rx_bytes,
                        tx_packets: stats.tx_packets,
                        tx_bytes: stats.tx_bytes,
                        tx_failed: stats.tx_failed,
                    },
                });
            }
        }

        let mut mirrors = Vec::new();
        if let Some(ref mirror) = server_info.mirror {
            for backend in mirror.backends() {
                let mirror_addr = backend.address().to_string();
                let stats = get_mirror_stats(&listener_addr, &mirror_addr);

                mirrors.push(MirrorStatus {
                    address: mirror_addr,
                    probability: backend.probability(),
                    stats: MirrorStats {
                        tx_packets: stats.tx_packets,
                        tx_bytes: stats.tx_bytes,
                        tx_failed: stats.tx_failed,
                    },
                });
            }
        }

        servers.push(ServerStatus {
            listener: listener_addr,
            algorithm,
            hash_key,
            stats: ListenerStats {
                rx_packets,
                rx_bytes,
                tx_packets,
                tx_bytes,
            },
            backends,
            mirrors,
        });
    }

    Json(StatusResponse {
        version: VERSION.to_string(),
        servers,
    })
}

async fn metrics_handler() -> Response<String> {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(gather_metrics())
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::balancer::{SendMode, UdpBackend};
    use crate::config::Algorithm as ConfigAlgorithm;
    use std::time::Duration;

    async fn create_test_balancer() -> Arc<Balancer> {
        let addr: SocketAddr = "127.0.0.1:9999".parse().unwrap();
        let backend = UdpBackend::new(addr, SendMode::Standard, 1.0, 1.0)
            .await
            .unwrap();
        Arc::new(Balancer::new(
            ConfigAlgorithm::Wrr,
            vec![Arc::new(backend)],
            vec![],
            ":2055".to_string(),
            Duration::from_secs(30),
        ))
    }

    #[tokio::test]
    async fn test_dashboard_response() {
        let response = dashboard_handler().await;
        assert!(response.0.contains("UDP Balancer"));
        assert!(response.0.contains("<!DOCTYPE html>"));
    }

    #[tokio::test]
    async fn test_ping_response() {
        let response = ping_handler().await;
        assert_eq!(response.0.status, "ok");
    }

    #[tokio::test]
    async fn test_status_response() {
        let balancer = create_test_balancer().await;
        let state = AppState {
            servers: vec![ServerInfo {
                balancer: Some(balancer),
                mirror: None,
            }],
        };

        let response = api_status_handler(State(state)).await;
        assert_eq!(response.0.servers.len(), 1);
        assert_eq!(response.0.servers[0].listener, ":2055");
        assert_eq!(response.0.servers[0].backends.len(), 1);
        assert!(!response.0.version.is_empty());
    }

    #[tokio::test]
    async fn test_metrics_response() {
        use crate::metrics::init_metrics;
        init_metrics();

        let response = metrics_handler().await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_status_server_creation() {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let server = StatusServer::new(addr, vec![]);
        assert_eq!(server.address, addr);
    }
}
