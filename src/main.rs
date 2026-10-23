use axum::{
    extract::{ConnectInfo, Path, Query},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::get,
    Router,
};
use std::collections::HashMap;
use std::io::Read;
use std::net::{SocketAddr, UdpSocket};
use std::process::Command;

const INDEX_HTML: &str = include_str!("../web/index.html");
const AGH_URL: &str = "http://127.0.0.1:3000";
const AGH_USER: &str = "root";
const AGH_PASS: &str = "20131400";

#[tokio::main]
async fn main() {
    let app = Router::new()
        .route("/", get(serve_index))
        .route("/api/client-ip", get(get_client_ip))
        .route("/control/*path", get(proxy_api).post(proxy_api));

    let addr = SocketAddr::from(([0, 0, 0, 0], 8888));
    
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            eprintln!("❌ 启动失败: 端口 8888 已被占用！");
            return;
        }
        Err(e) => {
            eprintln!("❌ 启动失败: {}", e);
            return;
        }
    };

    println!("✅ DNS 战报后端已启动: http://0.0.0.0:8888");

    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await.unwrap();
}

async fn serve_index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

// 获取手机真实的 IP (不管是 192.168, 10.x 还是 198.18 全部放行)
fn get_real_lan_ip() -> String {
    // 1. UDP 探测获取默认路由出口 IP
    if let Ok(socket) = UdpSocket::bind("0.0.0.0:0") {
        if socket.connect("223.5.5.5:53").is_ok() {
            if let Ok(addr) = socket.local_addr() {
                let ip = addr.ip().to_string();
                if ip != "0.0.0.0" && ip != "127.0.0.1" {
                    return ip;
                }
            }
        }
    }

    // 2. 如果 UDP 失败，直接读取网卡信息
    if let Ok(output) = Command::new("ip").args(&["-4", "addr"]).output() {
        let s = String::from_utf8_lossy(&output.stdout);
        for line in s.lines() {
            let line = line.trim();
            if line.starts_with("inet ") && !line.contains("127.0.0.1") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() > 1 && parts[1].contains('.') {
                    return parts[1].split('/').next().unwrap().to_string();
                }
            }
        }
    }
    
    "127.0.0.1".to_string()
}

async fn get_client_ip(ConnectInfo(addr): ConnectInfo<SocketAddr>) -> String {
    let ip = addr.ip().to_string();
    let clean_ip = ip.strip_prefix("::ffff:").unwrap_or(&ip).to_string();
    
    // 如果是手机本机访问，强行展示真实网卡 IP
    if clean_ip == "127.0.0.1" || clean_ip == "::1" {
        let real_ip = get_real_lan_ip();
        if real_ip != "127.0.0.1" {
            return real_ip;
        }
    }
    clean_ip
}

async fn proxy_api(
    Path(path): Path<String>, 
    Query(mut query): Query<HashMap<String, String>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>
) -> Response {
    let ip = addr.ip().to_string();
    let client_ip = ip.strip_prefix("::ffff:").unwrap_or(&ip).to_string();

    if path == "querylog" {
        if client_ip == "127.0.0.1" || client_ip == "::1" {
            // 核心逻辑：手机自己的 DNS 请求全在 127.0.0.1，必须用 127 查才能出数据
            query.insert("search".to_string(), "127.0.0.1".to_string());
        } else {
            // 局域网内的其他设备，用它们自己的真实 IP 查
            query.insert("search".to_string(), client_ip);
        }
    }

    let mut url = format!("{}/control/{}", AGH_URL, path);
    if !query.is_empty() {
        let qs: Vec<String> = query.iter().map(|(k, v)| format!("{}={}", k, urlencoding::encode(v))).collect();
        url = format!("{}?{}", url, qs.join("&"));
    }

    let result = tokio::task::spawn_blocking(move || {
        let login_body = format!(r#"{{"name":"{}","password":"{}"}}"#, AGH_USER, AGH_PASS);
        let login_resp = ureq::post(&format!("{}/control/login", AGH_URL))
            .set("Content-Type", "application/json")
            .send_string(&login_body);

        let mut cookie = String::new();
        if let Ok(resp) = login_resp {
            if let Some(c) = resp.header("set-cookie") {
                if let Some(session) = c.split(';').next() {
                    cookie = session.to_string();
                }
            }
        }

        let mut req = ureq::get(&url);
        if !cookie.is_empty() {
            req = req.set("Cookie", &cookie);
        }

        match req.call() {
            Ok(resp) => {
                let mut bytes = Vec::new();
                let _ = resp.into_reader().read_to_end(&mut bytes);
                Ok(bytes)
            }
            Err(e) => Err(e.to_string()),
        }
    }).await.unwrap();

    match result {
        Ok(body_bytes) => {
            let mut headers = HeaderMap::new();
            headers.insert("Content-Type", "application/json".parse().unwrap());
            (StatusCode::OK, headers, body_bytes).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Proxy error: {}", e)).into_response()
    }
}