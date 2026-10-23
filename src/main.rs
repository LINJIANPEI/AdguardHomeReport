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

// 核心：穷尽一切办法获取手机真实的局域网 IP
fn get_real_lan_ip() -> String {
    // 1. 尝试执行 ip addr show wlan0 获取 Wi-Fi 网卡 IP
    if let Ok(output) = Command::new("ip").args(&["addr", "show", "wlan0"]).output() {
        let s = String::from_utf8_lossy(&output.stdout);
        for line in s.lines() {
            let line = line.trim();
            if line.starts_with("inet ") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() > 1 {
                    let ip = parts[1];
                    if ip.contains('.') { // 过滤掉 IPv6 的地址
                        return ip.to_string();
                    }
                }
            }
        }
    }

    // 2. 如果 wlan0 获取失败，尝试通过路由表寻找真实的出口 src IP
    if let Ok(output) = Command::new("ip").args(&["route", "get", "223.5.5.5"]).output() {
        let s = String::from_utf8_lossy(&output.stdout);
        let parts: Vec<&str> = s.split_whitespace().collect();
        for i in 0..parts.len() {
            if parts[i] == "src" && i + 1 < parts.len() {
                return parts[i + 1].to_string();
            }
        }
    }

    // 3. 最后的兜底：UDP 探测（排除 Clash 等虚拟网卡）
    if let Ok(socket) = UdpSocket::bind("0.0.0.0:0") {
        if socket.connect("223.5.5.5:80").is_ok() {
            if let Ok(addr) = socket.local_addr() {
                let ip = addr.ip().to_string();
                if !ip.starts_with("198.18") && !ip.starts_with("198.19") && !ip.starts_with("10.") {
                    return ip;
                }
            }
        }
    }
    
    "未知".to_string()
}

// 返回给前端展示的 IP
async fn get_client_ip(ConnectInfo(addr): ConnectInfo<SocketAddr>) -> String {
    let ip = addr.ip().to_string();
    let clean_ip = ip.strip_prefix("::ffff:").unwrap_or(&ip).to_string();
    
    // 如果是手机本机访问，直接返回真实的 Wi-Fi IP
    if clean_ip == "127.0.0.1" || clean_ip == "::1" {
        let true_ip = get_real_lan_ip();
        if true_ip != "未知" {
            return true_ip;
        }
    }
    // 如果是电脑等其他设备访问，直接返回它们真实的局域网 IP
    clean_ip
}

async fn proxy_api(
    Path(path): Path<String>, 
    Query(mut query): Query<HashMap<String, String>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>
) -> Response {
    let ip = addr.ip().to_string();
    let client_ip = ip.strip_prefix("::ffff:").unwrap_or(&ip).to_string();

    // 核心逻辑：查日志时用什么 IP
    if path == "querylog" {
        if client_ip == "127.0.0.1" || client_ip == "::1" {
            // 手机自己查自己，AdGuard 日志里必须是 127.0.0.1
            query.insert("search".to_string(), "127.0.0.1".to_string());
        } else {
            // 局域网其他设备查自己，日志里是它们的局域网 IP
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