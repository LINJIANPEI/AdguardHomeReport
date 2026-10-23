use axum::{
    extract::{Path, Query},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::get,
    Router,
};
use std::collections::HashMap;
use std::net::SocketAddr;

const INDEX_HTML: &str = include_str!("../web/index.html");
const AGH_URL: &str = "http://127.0.0.1:3000";
const AGH_USER: &str = "root";
const AGH_PASS: &str = "20131400";

#[tokio::main]
async fn main() {
    let app = Router::new()
        .route("/", get(|| async { Html(INDEX_HTML) }))
        // 使用 axum 0.7 的路由参数捕获语法
        .route("/control/*path", get(proxy_api).post(proxy_api));

    let addr = SocketAddr::from(([127, 0, 0, 1], 8080));
    println!("Server running on http://{}", addr);
    
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn proxy_api(
    Path(path): Path<String>, 
    Query(query): Query<HashMap<String, String>>
) -> Response {
    // 创建带有 Cookie 存储的客户端
    let client = reqwest::Client::builder()
        .cookie_store(true)
        .build()
        .unwrap();

    // 1. 自动登录
    let _ = client
        .post(format!("{}/control/login", AGH_URL))
        .json(&serde_json::json!({ "name": AGH_USER, "password": AGH_PASS }))
        .send()
        .await;

    // 2. 拼接 URL
    let mut url = format!("{}/control/{}", AGH_URL, path);
    if !query.is_empty() {
        let qs: Vec<String> = query
            .iter()
            .map(|(k, v)| format!("{}={}", k, urlencoding::encode(v)))
            .collect();
        url = format!("{}?{}", url, qs.join("&"));
    }

    // 3. 请求后端 API
    match client.get(&url).send().await {
        Ok(resp) => {
            let mut headers = HeaderMap::new();
            headers.insert("Content-Type", "application/json".parse().unwrap());
            let status = resp.status();
            let bytes = resp.bytes().await.unwrap_or_default();
            (status, headers, bytes).into_response()
        }
        Err(e) => {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("Proxy error: {}", e)).into_response()
        }
    }
}