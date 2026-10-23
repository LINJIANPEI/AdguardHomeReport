use axum::{
    extract::{ConnectInfo, Path, Query},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::get,
    Router,
};
use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};

const INDEX_HTML: &str = include_str!("../web/index.html");
const AGH_URL: &str = "http://127.0.0.1:3000";
const AGH_USER: &str = "root";
const AGH_PASS: &str = "20131400";

#[tokio::main]
async fn main() {
    let app = Router::new()
        .route("/", get(serve_index))
        .route("/api/local-ip", get(get_local_ip))
        .route("/api/client-ip", get(get_client_ip))
        .route("/control/*path", get(proxy_api).post(proxy_api));

    let addr = SocketAddr::from(([0, 0, 0, 0], 8888));
    
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            eprintln!("❌ 启动失败: 端口 8888 已被占用！");
            eprintln!("👉 请先执行 'killall dns-report' 杀掉旧进程。");
            return;
        }
        Err(e) => {
            eprintln!("❌ 启动失败: {}", e);
            return;
        }
    };

    println!("✅ DNS 战报后端已启动: http://0.0.0.0:{}", 8888);

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

async fn get_local_ip() -> String {
    let socket = match UdpSocket::bind("0.0.0.0:0") {
        Ok(s) => s,
        Err(_) => return "127.0.0.1".to_string(),
    };
    if socket.connect("8.8.8.8:80").is_ok() {
        if let Ok(addr) = socket.local_addr() {
            return addr.ip().to_string();
        }
    }
    "127.0.0.1".to_string()
}

async fn get_client_ip(ConnectInfo(addr): ConnectInfo<SocketAddr>) -> String {
    let ip = addr.ip().to_string();
    if ip.starts_with("::ffff:") {
        return ip.replace("::ffff:", "");
    }
    ip
}

async fn proxy_api(
    Path(path): Path<String>, 
    Query(mut query): Query<HashMap<String, String>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>
) -> Response {
    let client_ip = addr.ip().to_string();
    let client_ip = client_ip.strip_prefix("::ffff:").unwrap_or(&client_ip).to_string();

    if path == "querylog" {
        query.insert("search".to_string(), client_ip);
    }

    let client = reqwest::Client::builder()
        .cookie_store(true)
        .build()
        .unwrap();

    let login_body = serde_json::json!({ "name": AGH_USER, "password": AGH_PASS });
    let _ = client.post(format!("{}/control/login", AGH_URL))
        .json(&login_body)
        .send()
        .await;

    let mut url = format!("{}/control/{}", AGH_URL, path);
    if !query.is_empty() {
        let qs: Vec<String> = query.iter().map(|(k, v)| format!("{}={}", k, urlencoding::encode(v))).collect();
        url = format!("{}?{}", url, qs.join("&"));
    }

    let res = client.get(&url).send().await;

    match res {
        Ok(resp) => {
            let status = resp.status();
            let body_bytes = resp.bytes().await.unwrap_or_default();
            let mut headers = HeaderMap::new();
            headers.insert("Content-Type", "application/json".parse().unwrap());
            (status, headers, body_bytes).into_response()
        }
        Err(e) => {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("Proxy error: {}", e)).into_response()
        }
    }
}