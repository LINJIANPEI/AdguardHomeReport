use axum::{
    extract::{ConnectInfo, Path, Query},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::get,
    Router,
};
use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};

// 将前端网页直接打包进二进制文件中
const INDEX_HTML: &str = include_str!("../web/index.html");

// AdGuard 的本机地址与账号密码
const AGH_URL: &str = "http://127.0.0.1:3000";
const AGH_USER: &str = "root";
const AGH_PASS: &str = "20131400";

#[tokio::main]
async fn main() {
    let app = Router::new()
        .route("/", get(serve_index))
        .route("/api/local-ip", get(get_local_ip))
        .route("/api/client-ip", get(get_client_ip)) // 新增：获取当前访问者 IP
        .route("/control/*path", get(proxy_api).post(proxy_api));

    // ⚠️ 监听 0.0.0.0 允许局域网内其他设备访问，端口设为 8888
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
    println!("👉 手机访问: http://127.0.0.1:8888");
    println!("👉 局域网访问: http://<你的手机局域网IP>:8888");

    // 必须使用 into_make_service_with_connect_info 才能获取客户端真实 IP
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await.unwrap();
}

// 返回打包好的 HTML
async fn serve_index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

// 获取手机自身的局域网 IP
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

// 获取当前访问浏览器的局域网 IP
async fn get_client_ip(ConnectInfo(addr): ConnectInfo<SocketAddr>) -> String {
    let ip = addr.ip().to_string();
    if ip.starts_with("::ffff:") {
        return ip.replace("::ffff:", "");
    }
    ip
}

// 核心反代：自动登录拿 Cookie -> 转发请求 -> 返回结果
async fn proxy_api(
    Path(path): Path<String>, 
    Query(mut query): Query<HashMap<String, String>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>
) -> Response {
    let client_ip = addr.ip().to_string();
    let client_ip = client_ip.strip_prefix("::ffff:").unwrap_or(&client_ip).to_string();

    // 💡 核心逻辑：如果请求的是 querylog，强制把搜索的 IP 替换为当前访问者的真实 IP
    if path == "querylog" {
        query.insert("search".to_string(), client_ip);
    }

    let client = reqwest::Client::builder()
        .cookie_store(true)
        .build()
        .unwrap();

    // 1. 尝试登录获取 Cookie
    let login_body = serde_json::json!({ "name": AGH_USER, "password": AGH_PASS });
    let _ = client.post(format!("{}/control/login", AGH_URL))
        .json(&login_body)
        .send()
        .await;

    // 2. 拼接目标 URL
    let mut url = format!("{}/control/{}", AGH_URL, path);
    if !query.is_empty() {
        let qs: Vec<String> = query.iter().map(|(k, v)| format!("{}={}", k, urlencoding::encode(v))).collect();
        url = format!("{}?{}", url, qs.join("&"));
    }

    // 3. 请求真实接口
    let res = client.get(&url).send().await;

    // 4. 返回数据
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