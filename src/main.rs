use axum::{
    extract::{ConnectInfo, Path, Query},
    http::{header, HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    env,
    io::Read,
    net::{IpAddr, SocketAddr, UdpSocket},
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

/// 通过路由探测获得“本机到目标网络时系统选中的源地址”。
/// 这不是公网出口 IP，也不一定是浏览器实际使用的地址。
fn route_source_ip(target: &str, bind_addr: &str) -> Option<IpAddr> {
    let socket = UdpSocket::bind(bind_addr).ok()?;
    socket.set_write_timeout(Some(Duration::from_secs(1))).ok()?;
    socket.connect(target).ok()?;
    let ip = socket.local_addr().ok()?.ip();
    if ip.is_unspecified() || ip.is_loopback() {
        None
    } else {
        Some(ip)
    }
}

fn route_source_ipv4() -> Option<IpAddr> {
    route_source_ip("223.5.5.5:53", "0.0.0.0:0")
}

fn route_source_ipv6() -> Option<IpAddr> {
    route_source_ip("[2400:3200::1]:53", "[::]:0")
}

fn trusted_proxy(peer: IpAddr) -> bool {
    // 只有明确配置在 TRUSTED_PROXY_IPS 中的反向代理才可信。
    // 例如：TRUSTED_PROXY_IPS=127.0.0.1,::1,192.168.1.10
    let configured = env::var("TRUSTED_PROXY_IPS").unwrap_or_default();
    configured
        .split(',')
        .map(str::trim)
        .filter_map(|value| value.parse::<IpAddr>().ok())
        .any(|ip| ip == peer)
}

fn parse_forwarded_ip(headers: &HeaderMap, peer: IpAddr) -> Option<IpAddr> {
    if !trusted_proxy(peer) {
        return None;
    }

    // 只在 TCP 对端是显式信任的代理时读取转发头。
    // 优先 X-Forwarded-For 最左侧地址，再尝试 Cloudflare 专用头。
    let forwarded = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .and_then(|v| v.trim().parse::<IpAddr>().ok());

    forwarded.or_else(|| {
        headers
            .get("cf-connecting-ip")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<IpAddr>().ok())
    })
}

async fn get_client_ip(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Json<Value> {
    let peer_ip = peer.ip();
    let forwarded_ip = parse_forwarded_ip(&headers, peer_ip);
    let local_v4 = route_source_ipv4();
    let local_v6 = route_source_ipv6();

    // query_ips 是查询 AdGuard Home 日志时使用的候选地址，不宣称每个地址都一定出现在日志中。
    // 这些地址是本机路由源地址；可信代理来源仅在明确配置 TRUSTED_PROXY_IPS 时加入。
    let mut query_ips: Vec<String> = Vec::new();
    if let Some(ip) = local_v4 {
        query_ips.push(ip.to_string());
    }
    if let Some(ip) = local_v6 {
        query_ips.push(ip.to_string());
    }
    if let Some(ip) = forwarded_ip {
        query_ips.push(ip.to_string());
    }

    // 代理通过本机重定向 53 端口时，AdGuard Home 可能记录回环地址，
    // 也可能记录 VPN/网络接口地址。它们是日志匹配候选，不是公网出口 IP。
    query_ips.push("127.0.0.1".to_string());
    query_ips.push("::1".to_string());

    // 如果页面从局域网另一台设备访问，TCP 对端也可能是日志中的客户端地址。
    if !peer_ip.is_unspecified() {
        query_ips.push(peer_ip.to_string());
    }
    query_ips.sort();
    query_ips.dedup();

    let family = match peer_ip {
        IpAddr::V4(_) => "IPv4",
        IpAddr::V6(_) => "IPv6",
    };

    Json(json!({
        "connection_ip": peer_ip.to_string(),
        "connection_family": family,
        "local_ipv4": local_v4.map(|ip| ip.to_string()),
        "local_ipv6": local_v6.map(|ip| ip.to_string()),
        "forwarded_ip": forwarded_ip.map(|ip| ip.to_string()),
        "trusted_proxy": trusted_proxy(peer_ip),
        "query_ips": query_ips
    }))
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
            Json(json!({ "error": message })),
        )
            .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("代理任务失败：{error}") })),
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
    let login_body = json!({
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
        let encoded = query
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

    let status = StatusCode::from_u16(response.status()).unwrap_or(StatusCode::OK);
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
