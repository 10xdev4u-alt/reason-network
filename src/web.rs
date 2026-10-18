//! A small local web dashboard for Reason Network: team directory, sessions, inbox, held messages and send.
//! Served by the daemon over HTTP on 127.0.0.1 by default. Vanilla JS frontend, polling + SSE triggers.

use crate::daemon::Daemon;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

/// An unbounded header line, header count or body is a way to make the daemon allocate whatever an
/// attacker likes, so each has a ceiling.
const MAX_HEADER_BYTES: usize = 8 * 1024;
const MAX_HEADERS: usize = 100;
const MAX_BODY: usize = 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

struct Req {
    method: String,
    path: String,
    headers: std::collections::HashMap<String, String>,
    body: String,
}

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
        let host = bind.to_string();
        tokio::spawn(async move {
            if let Err(e) = handle(stream, d, tok, &host, port).await {
                let _ = e;
            }
        });
    }
}

async fn handle(
    stream: tokio::net::TcpStream,
    daemon: Arc<Daemon>,
    token: Option<String>,
    bind: &str,
    port: u16,
) -> std::io::Result<()> {
    let (rd, mut wr) = stream.into_split();
    let mut reader = BufReader::new(rd);
    let req = match tokio::time::timeout(REQUEST_TIMEOUT, read_request(&mut reader)).await {
        Ok(Ok(Some(r))) => r,
        _ => {
            write_response(&mut wr, 400, "application/json", r#"{"error":"bad request"}"#).await?;
            return Ok(());
        }
    };
    let path_only = req.path.split('?').next().unwrap_or(&req.path).to_string();

    // Every client must name the host it believes it is talking to. Only names that can legitimately
    // resolve to this listener pass, which is what breaks DNS rebinding.
    if !host_allowed(header(&req.headers, "host"), bind, port) {
        write_response(&mut wr, 403, "application/json", r#"{"error":"bad host"}"#).await?;
        return Ok(());
    }
    // A cross-origin HTML form cannot send `application/json` without a preflight, and no preflight is
    // granted, so requiring the JSON content type on every mutating route is what removes the
    // cross-origin write primitive. Origin, when a client sends one, must agree.
    if req.method == "POST" {
        let ct = header(&req.headers, "content-type").unwrap_or("");
        if !ct.starts_with("application/json") {
            write_response(&mut wr, 415, "application/json", r#"{"error":"content-type must be application/json"}"#)
                .await?;
            return Ok(());
        }
        if let Some(site) = header(&req.headers, "sec-fetch-site")
            && site != "same-origin"
            && site != "none"
        {
            write_response(&mut wr, 403, "application/json", r#"{"error":"cross-site write refused"}"#).await?;
            return Ok(());
        }
        if let Some(o) = header(&req.headers, "origin")
            && !origin_allowed(o, bind, port)
        {
            write_response(&mut wr, 403, "application/json", r#"{"error":"bad origin"}"#).await?;
            return Ok(());
        }
    }
    // A token in a URL ends up in history, logs and referrers, so it is accepted once and exchanged for
    // a cookie. `EventSource` cannot set a request header, which is why a cookie is the only credential
    // that works for the event stream.
    if let Some(t) = &token {
        if query_param(&req.path, "token").as_deref() == Some(t.as_str()) {
            write_redirect(&mut wr, &path_only, &format!("rn={t}; HttpOnly; SameSite=Strict; Path=/")).await?;
            return Ok(());
        }
        let from_cookie = header(&req.headers, "cookie")
            .map(|c| c.split(';').any(|kv| kv.trim() == format!("rn={t}")))
            .unwrap_or(false);
        let from_header = header(&req.headers, "authorization").map(|v| v == format!("Bearer {t}")).unwrap_or(false);
        if !from_cookie && !from_header {
            let msg = "this dashboard needs a token: open /?token=<token> once and it will not ask again";
            if path_only == "/" || path_only == "/index.html" {
                write_response(&mut wr, 401, "text/html; charset=utf-8", &format!("<!doctype html><p>{msg}</p>"))
                    .await?;
            } else {
                write_json(&mut wr, 401, &json!({ "error": msg })).await?;
            }
            return Ok(());
        }
    }

    let body_str = &req.body;
    match (req.method.as_str(), path_only.as_str()) {
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
            let address = query_param(&req.path, "address").unwrap_or_default();
            match daemon.inbox_json(&address).await {
                Ok(v) => write_json(&mut wr, 200, &v).await?,
                Err(e) => write_response(&mut wr, 404, "application/json", &format!(r#"{{"error":"{e}"}}"#)).await?,
            }
        }
        ("POST", "/api/send") => {
            // parse minimal JSON fields
            let v: serde_json::Value = serde_json::from_str(body_str).unwrap_or(json!({}));
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
            let v: serde_json::Value = serde_json::from_str(body_str).unwrap_or(json!({}));
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

fn header<'a>(headers: &'a std::collections::HashMap<String, String>, key: &str) -> Option<&'a str> {
    headers.get(key).map(|s| s.as_str())
}

fn is_loopback(bind: &str) -> bool {
    bind == "127.0.0.1" || bind == "::1" || bind == "localhost"
}

/// Accept only the names that can legitimately reach this listener.
fn host_allowed(host: Option<&str>, bind: &str, port: u16) -> bool {
    let Some(h) = host else { return false };
    let (name, declared_port) = match h.rsplit_once(':') {
        Some((n, p)) if p.chars().all(|c| c.is_ascii_digit()) => (n, Some(p)),
        _ => (h, None),
    };
    if let Some(p) = declared_port
        && p.parse::<u16>().ok() != Some(port)
    {
        return false;
    }
    let name = name.trim_start_matches('[').trim_end_matches(']');
    if name == bind || name == "127.0.0.1" || name == "localhost" || name == "::1" {
        return true;
    }
    // A listener on a routable address cannot know which name reaches it; its boundary is the token.
    !is_loopback(bind)
}

fn origin_allowed(origin: &str, bind: &str, port: u16) -> bool {
    match origin.strip_prefix("http://") {
        Some(rest) => host_allowed(Some(rest), bind, port),
        None => false,
    }
}

/// One header line, with a ceiling. Without it a client decides how much the daemon buffers before it sees a
/// newline, so the cap lives on the read, not on the line it already read.
async fn read_capped_line(
    reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
    buf: &mut String,
) -> std::io::Result<usize> {
    let mut limited = reader.take(MAX_HEADER_BYTES as u64);
    limited.read_line(buf).await
}

async fn read_request(reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>) -> std::io::Result<Option<Req>> {
    let mut line = String::new();
    if read_capped_line(reader, &mut line).await? == 0 || !line.ends_with('\n') {
        return Ok(None);
    }
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 2 {
        return Ok(None);
    }
    let (method, path) = (parts[0].to_string(), parts[1].to_string());
    let mut headers = std::collections::HashMap::new();
    let mut content_length = 0usize;
    let mut head_end = false;
    for _ in 0..MAX_HEADERS {
        line.clear();
        if read_capped_line(reader, &mut line).await? == 0 || !line.ends_with('\n') {
            return Ok(None);
        }
        let trimmed = line.trim_end_matches("\r\n").trim_end_matches('\n');
        if trimmed.is_empty() {
            head_end = true;
            break;
        }
        if let Some((k, v)) = trimmed.split_once(':') {
            let k = k.trim().to_ascii_lowercase();
            let v = v.trim().to_string();
            if k == "content-length" {
                content_length = v.parse().unwrap_or(0);
                if content_length > MAX_BODY {
                    return Ok(None);
                }
            }
            headers.insert(k, v);
        }
    }
    // Running out of the header budget is a request to allocate, not a request: refuse it rather than
    // treat the remaining headers as a body.
    if !head_end {
        return Ok(None);
    }
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body).await?;
    }
    Ok(Some(Req { method, path, headers, body: String::from_utf8_lossy(&body).into_owned() }))
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
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 3 <= b.len() => {
                let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok());
                match hex {
                    Some(v) => {
                        out.push(v);
                        i += 3;
                    }
                    None => {
                        out.push(b[i]);
                        i += 1;
                    }
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
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
        403 => "Forbidden",
        404 => "Not Found",
        415 => "Unsupported Media Type",
        500 => "Internal Server Error",
        _ => "OK",
    };
    // `default-src 'none'` with an explicit allowance is what keeps a message body from becoming code:
    // the page loads its own script and stylesheet and nothing else.
    let resp = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nX-Content-Type-Options: nosniff\r\n\
         Referrer-Policy: no-referrer\r\n\
         Content-Security-Policy: default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'\r\n\
         Connection: close\r\n\r\n{}",
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

async fn write_redirect(wr: &mut tokio::net::tcp::OwnedWriteHalf, location: &str, cookie: &str) -> std::io::Result<()> {
    let resp = format!(
        "HTTP/1.1 302 Found\r\nLocation: {location}\r\nSet-Cookie: {cookie}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
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
