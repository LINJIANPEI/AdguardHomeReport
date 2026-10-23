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

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .unwrap();
}

async fn serve_index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

// 获取手机当前的物理 IP，如果被代理接管，则返回代理的虚拟 IP（比如 198.18.0.1）
fn get_real_lan_ip() -> String {
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
    "127.0.0.1".to_string()
}

async fn get_client_ip(ConnectInfo(addr): ConnectInfo<SocketAddr>) -> String {
    let ip = addr.ip().to_string();
    let clean_ip = ip.strip_prefix("::ffff:").unwrap_or(&ip).to_string();

    // 如果本机访问，返回真实的网卡 IP（如 198.18.0.1）
    if clean_ip == "127.0.0.1" || clean_ip == "::1" {
        return get_real_lan_ip();
    }
    clean_ip
}

async fn proxy_api(
    Path(path): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    ConnectInfo(_addr): ConnectInfo<SocketAddr>,
) -> Response {
    // ✨ 核心：完全信任前端传来的 search 参数（传 198.18.0.1 就查 198.18.0.1）
    let mut url = format!("{}/control/{}", AGH_URL, path);
    if !query.is_empty() {
        let qs: Vec<String> = query
            .iter()
            .map(|(k, v)| format!("{}={}", k, urlencoding::encode(v)))
            .collect();
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
    })
    .await
    .unwrap();

    match result {
        Ok(body_bytes) => {
            let mut headers = HeaderMap::new();
            headers.insert("Content-Type", "application/json".parse().unwrap());
            (StatusCode::OK, headers, body_bytes).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Proxy error: {}", e),
        )
            .into_response(),
    }
}
