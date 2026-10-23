use axum::{
    extract::{ConnectInfo, Path, Query},
    http::{header, HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde_json::Value;
use std::{
    collections::HashMap,
    io::Read,
    net::{SocketAddr, UdpSocket},
    time::Duration,
};

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
        Ok(listener) => listener,
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
            eprintln!("启动失败：端口 8888 已被占用。");
            return;
        }
        Err(error) => {
            eprintln!("启动失败：{error}");
            return;
        }
    };

    println!("DNS 战报后端已启动：http://0.0.0.0:8888");
    if let Err(error) = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    {
        eprintln!("HTTP 服务退出：{error}");
    }
}

async fn serve_index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

fn probe_local_ip(target: &str) -> Option<String> {
    let socket = UdpSocket::bind(if target.contains('[') || target.contains(':') {
        "[::]:0"
    } else {
        "0.0.0.0:0"
    })
    .ok()?;
    socket.set_write_timeout(Some(Duration::from_secs(1))).ok()?;
    socket.connect(target).ok()?;
    Some(socket.local_addr().ok()?.ip().to_string())
}

fn get_active_dual_stack_ips() -> Vec<String> {
    let mut ips = Vec::new();

    if let Some(ip) = probe_local_ip("223.5.5.5:53") {
        if !ip.starts_with("127.") && ip != "0.0.0.0" {
            ips.push(ip);
        }
    }
    if let Some(ip) = probe_local_ip("[2400:3200::1]:53") {
        if !ip.starts_with("::") && ip != "::" {
            ips.push(ip);
        }
    }

    if ips.is_empty() {
        ips.push("127.0.0.1".to_string());
        ips.push("::1".to_string());
    }
    ips.sort();
    ips.dedup();
    ips
}

async fn get_client_ip(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Json<Value> {
    let mut ips = get_active_dual_stack_ips();

    // 若浏览器直接连接本服务且客户端地址是有效公网/局域网地址，也将其纳入候选。
    let peer = addr.ip();
    if !peer.is_unspecified() && !peer.is_loopback() {
        let peer_string = peer.to_string();
        if !ips.contains(&peer_string) {
            ips.push(peer_string);
        }
    }

    Json(serde_json::json!({ "ips": ips }))
}

async fn proxy_api(
    Path(path): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    _headers: HeaderMap,
) -> Response {
    match tokio::task::spawn_blocking(move || proxy_api_blocking(path, query)).await {
        Ok(Ok((status, content_type, body))) => {
            let mut response = Response::new(axum::body::Body::from(body));
            *response.status_mut() = status;
            if let Ok(value) = header::HeaderValue::from_str(&content_type) {
                response.headers_mut().insert(header::CONTENT_TYPE, value);
            }
            response
        }
        Ok(Err(message)) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({ "error": message })),
        )
            .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("代理任务失败：{error}") })),
        )
            .into_response(),
    }
}

fn proxy_api_blocking(
    path: String,
    query: HashMap<String, String>,
) -> Result<(StatusCode, String, Vec<u8>), String> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(20))
        .timeout_write(Duration::from_secs(10))
        .build();

    let login_url = format!("{AGH_URL}/control/login");
    let login_body = serde_json::json!({
        "name": AGH_USER,
        "password": AGH_PASS
    });

    let login_response = agent
        .post(&login_url)
        .set("Content-Type", "application/json")
        .send_string(&login_body.to_string());

    let mut cookie = String::new();
    match login_response {
        Ok(response) => {
            if let Some(value) = response.header("set-cookie") {
                cookie = value.split(';').next().unwrap_or(value).to_string();
            }
        }
        Err(ureq::Error::Status(code, response)) => {
            return Err(format!(
                "AdGuard Home 登录失败，HTTP {code}：{}",
                response.status_text()
            ));
        }
        Err(error) => return Err(format!("无法连接 AdGuard Home 登录接口：{error}")),
    }

    let mut url = format!("{AGH_URL}/control/{path}");
    if !query.is_empty() {
        let encoded: String = query
            .iter()
            .map(|(key, value)| {
                format!(
                    "{}={}",
                    urlencoding::encode(key),
                    urlencoding::encode(value)
                )
            })
            .collect::<Vec<_>>()
            .join("&");
        url.push('?');
        url.push_str(&encoded);
    }

    let mut request = agent.get(&url);
    if !cookie.is_empty() {
        request = request.set("Cookie", &cookie);
    }

    let response = match request.call() {
        Ok(response) => response,
        Err(ureq::Error::Status(code, response)) => {
            let mut body = Vec::new();
            let _ = response.into_reader().read_to_end(&mut body);
            return Ok((
                StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_GATEWAY),
                "application/json; charset=utf-8".to_string(),
                body,
            ));
        }
        Err(error) => return Err(format!("请求 AdGuard Home API 失败：{error}")),
    };

    let status = StatusCode::from_u16(response.status())
        .unwrap_or(StatusCode::OK);
    let content_type = response
        .header("content-type")
        .unwrap_or("application/json; charset=utf-8")
        .to_string();

    let mut body = Vec::new();
    response
        .into_reader()
        .read_to_end(&mut body)
        .map_err(|error| format!("读取 AdGuard Home 响应失败：{error}"))?;

    Ok((status, content_type, body))
}
