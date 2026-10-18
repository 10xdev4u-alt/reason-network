//! A small local web dashboard for Reason Network: team directory, sessions, inbox, held messages and send.
//! Served by the daemon over HTTP on 127.0.0.1 by default. Vanilla JS frontend, polling + SSE triggers.

use crate::daemon::Daemon;
use serde_json::json;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

pub async fn run(daemon: Arc<Daemon>, bind: &str, port: u16, token: Option<String>) {
    let addr = format!("{bind}:{port}");
    let listener = match TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[web] could not bind {addr}: {e}");
            return;
        }
    };
    eprintln!("[web] dashboard at http://{addr}");
    loop {
        let (stream, _) = match listener.accept().await {
            Ok(x) => x,
            Err(_) => continue,
        };
        let d = daemon.clone();
        let tok = token.clone();
        tokio::spawn(async move {
            if let Err(e) = handle(stream, d, tok).await {
                let _ = e;
            }
        });
    }
}

async fn handle(stream: tokio::net::TcpStream, daemon: Arc<Daemon>, token: Option<String>) -> std::io::Result<()> {
    let (rd, mut wr) = stream.into_split();
    let mut reader = BufReader::new(rd);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).await?;
    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() < 2 {
        return Ok(());
    }
    let method = parts[0];
    let path = parts[1];
    // Read headers
    let mut headers = std::collections::HashMap::new();
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        let line = line.trim_end_matches("\r\n").trim_end_matches('\n');
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            let k = k.trim().to_ascii_lowercase();
            let v = v.trim().to_string();
            if k == "content-length" {
                content_length = v.parse().unwrap_or(0);
            }
            headers.insert(k, v);
        }
    }
    // Auth check
    if let Some(t) = &token {
        let ok = headers.get("authorization").map(|v| v == &format!("Bearer {t}")).unwrap_or(false)
            || path.contains(&format!("token={t}"));
        if !ok {
            write_response(&mut wr, 401, "application/json", r#"{"error":"unauthorized"}"#).await?;
            return Ok(());
        }
    }
    // Read body
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body).await?;
    }
    let body_str = String::from_utf8_lossy(&body);

    // Route
    let path_only = path.split('?').next().unwrap_or(path);
    match (method, path_only) {
        ("GET", "/") | ("GET", "/index.html") => {
            write_response(&mut wr, 200, "text/html; charset=utf-8", INDEX_HTML).await?;
        }
        ("GET", "/bundle.css") => {
            write_response(&mut wr, 200, "text/css; charset=utf-8", BUNDLE_CSS).await?;
        }
        ("GET", "/page.js") => {
            write_response(&mut wr, 200, "application/javascript; charset=utf-8", PAGE_JS).await?;
        }
        ("GET", "/api/who") => {
            let me = None;
            match daemon.who(me, None, false).await {
                Ok(d) => write_json(&mut wr, 200, &d).await?,
                Err(e) => write_response(&mut wr, 500, "application/json", &format!(r#"{{"error":"{e}"}}"#)).await?,
            }
        }
        ("GET", "/api/team") => match daemon.team_info() {
            Ok(t) => write_json(&mut wr, 200, &t).await?,
            Err(e) => write_response(&mut wr, 500, "application/json", &format!(r#"{{"error":"{e}"}}"#)).await?,
        },
        ("GET", "/api/machines") => {
            let m = daemon.machines();
            write_json(&mut wr, 200, &m).await?;
        }
        ("GET", "/api/held") => {
            let msgs = daemon.inner_held();
            write_json(&mut wr, 200, &msgs).await?;
        }
        ("GET", "/api/inbox") => {
            let key = query_param(path, "key").unwrap_or_default();
            match daemon.inbox_json(&key).await {
                Ok(v) => write_json(&mut wr, 200, &v).await?,
                Err(e) => write_response(&mut wr, 500, "application/json", &format!(r#"{{"error":"{e}"}}"#)).await?,
            }
        }
        ("POST", "/api/send") => {
            // parse minimal JSON fields
            let v: serde_json::Value = serde_json::from_str(&body_str).unwrap_or(json!({}));
            let from = v.get("from").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let to = v.get("to").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let body = v.get("body").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let notify = v.get("notify").and_then(|x| x.as_bool()).unwrap_or(false);
            match daemon.send_web(from, &to, &body, notify).await {
                Ok(r) => write_json(&mut wr, 200, &r).await?,
                Err(e) => write_response(&mut wr, 400, "application/json", &format!(r#"{{"error":"{e}"}}"#)).await?,
            }
        }
        ("POST", "/api/settle") => {
            let v: serde_json::Value = serde_json::from_str(&body_str).unwrap_or(json!({}));
            let ids: Vec<String> = v
                .get("ids")
                .and_then(|x| x.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            let release = v.get("release").and_then(|x| x.as_bool()).unwrap_or(true);
            let n = daemon.settle_web(&ids, release);
            write_json(&mut wr, 200, &json!({"n": n})).await?;
        }
        ("GET", "/api/events") => {
            // SSE stream
            wr.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n").await?;
            wr.flush().await?;
            let notify = daemon.arrived();
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(25));
            loop {
                tokio::select! {
                    _ = notify.notified() => {
                        if wr.write_all(b"data: {\"type\":\"changed\"}\n\n").await.is_err() { break; }
                        let _ = wr.flush().await;
                    }
                    _ = ticker.tick() => {
                        if wr.write_all(b": heartbeat\n\n").await.is_err() { break; }
                        let _ = wr.flush().await;
                    }
                }
            }
        }
        _ => {
            write_response(&mut wr, 404, "application/json", r#"{"error":"not found"}"#).await?;
        }
    }
    Ok(())
}

fn query_param(path: &str, key: &str) -> Option<String> {
    let q = path.split('?').nth(1)?;
    for pair in q.split('&') {
        let mut kv = pair.splitn(2, '=');
        if kv.next() == Some(key) {
            return kv.next().map(url_decode);
        }
    }
    None
}

fn url_decode(s: &str) -> String {
    s.replace('+', " ")
}

async fn write_response(
    wr: &mut tokio::net::tcp::OwnedWriteHalf,
    status: u16,
    ct: &str,
    body: &str,
) -> std::io::Result<()> {
    let status_text = match status {
        200 => "OK",
        401 => "Unauthorized",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "OK",
    };
    let resp = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        status,
        status_text,
        ct,
        body.len(),
        body
    );
    wr.write_all(resp.as_bytes()).await?;
    wr.flush().await?;
    Ok(())
}

async fn write_json<T: serde::Serialize>(
    wr: &mut tokio::net::tcp::OwnedWriteHalf,
    status: u16,
    v: &T,
) -> std::io::Result<()> {
    let body = serde_json::to_string(v).unwrap_or_default();
    write_response(wr, status, "application/json", &body).await
}

// Helper methods on Daemon (called from web module). These are added as pub methods.

// -- embedded assets --
const INDEX_HTML: &str = include_str!("web/index.html");
const BUNDLE_CSS: &str = include_str!("web/bundle.css");
const PAGE_JS: &str = include_str!("web/page.js");
