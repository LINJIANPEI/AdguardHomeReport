use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

const INDEX_HTML: &str = include_str!("../web/index.html");
const AGH_PORT: u16 = 3000;

fn main() {
    let listener = TcpListener::bind("127.0.0.1:8080").unwrap();
    println!("Server running on http://127.0.0.1:8080");

    for stream in listener.incoming() {
        if let Ok(stream) = stream {
            thread::spawn(|| {
                handle_connection(stream);
            });
        }
    }
}

fn handle_connection(mut client: TcpStream) {
    let mut buffer = [0; 4096];
    if client.read(&mut buffer).unwrap_or(0) == 0 {
        return;
    }
    let req_str = String::from_utf8_lossy(&buffer);

    // 解析请求路径
    let first_line = req_str.lines().next().unwrap_or("");
    let path = first_line.split_whitespace().nth(1).unwrap_or("/");

    if path == "/" || path == "/index.html" {
        // 返回 HTML
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            INDEX_HTML.len(),
            INDEX_HTML
        );
        let _ = client.write_all(response.as_bytes());
        return;
    }

    if path.starts_with("/control/") {
        // 1. 向 AdGuard 登录获取 Cookie
        let mut agh_conn = TcpStream::connect(format!("127.0.0.1:{}", AGH_PORT)).unwrap();
        let login_body = r#"{"name":"root","password":"20131400"}"#;
        let login_req = format!(
            "POST /control/login HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n{}",
            AGH_PORT, login_body.len(), login_body
        );
        let _ = agh_conn.write_all(login_req.as_bytes());

        let mut agh_buf = [0; 4096];
        let n = agh_conn.read(&mut agh_buf).unwrap_or(0);
        let login_resp = String::from_utf8_lossy(&agh_buf[..n]);

        // 提取 Cookie
        let mut cookie = String::new();
        for line in login_resp.lines() {
            if line.to_lowercase().starts_with("set-cookie:") {
                cookie = line["set-cookie:".len()..]
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                break;
            }
        }

        // 2. 使用 Cookie 请求真实数据
        let mut data_conn = TcpStream::connect(format!("127.0.0.1:{}", AGH_PORT)).unwrap();
        let data_req = format!(
            "GET {} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nCookie: {}\r\nConnection: close\r\n\r\n",
            path, AGH_PORT, cookie
        );
        let _ = data_conn.write_all(data_req.as_bytes());

        let mut data_buf = Vec::new();
        let _ = data_conn.read_to_end(&mut data_buf);

        // 3. 原样返回给浏览器
        let _ = client.write_all(&data_buf);
        return;
    }

    // 404
    let _ = client.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
}
