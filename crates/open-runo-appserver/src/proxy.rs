//! HTTP/1.1 転送プロキシ(Dispatcher の実転送レイヤ、Phase 2)。
//!
//! 依存追加なし(`std::net` のみ)で動く同期実装。sandbox の cargo 1.75 でも
//! 単独検証できることを優先し、Poem/tokio 統合はこの関数群を呼ぶ薄い
//! ハンドラとして各リポジトリ側(open-web-server の app_proxy、
//! poem-cosmo-tauri の gateway)に置く(§0.9.3)。
//!
//! 対応範囲(Phase 2):
//! - リクエストヘッダの読み取り、`Content-Length` ボディの中継
//! - `Host` ヘッダの upstream 向け書き換え + `X-Forwarded-Host`/`X-Forwarded-For` 付与
//! - レスポンスの素通し(status line + headers + Content-Length ボディ)
//! - 非対応: chunked transfer-encoding、WebSocket/upgrade、keep-alive
//!   (1リクエスト=1接続で処理し `Connection: close` を強制)——Phase 3 で拡張

use crate::{Dispatcher, UpstreamAddr};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

/// ヘッダ部合計の上限(ヘッダ爆弾対策)。
pub const MAX_HEADER_BYTES: usize = 16 * 1024;
/// ボディの上限(メモリ枯渇対策)。金融系ペイロードには十分。
pub const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug)]
pub enum ProxyError {
    /// Host ヘッダが無い、または行が不正。
    BadRequest(&'static str),
    /// ヘッダ/ボディがサイズ上限を超過(攻撃または設定ミス)。
    TooLarge(&'static str),
    /// Dispatcher が host を解決できなかった。
    NoRoute(String),
    Io(std::io::Error),
}

impl From<std::io::Error> for ProxyError {
    fn from(e: std::io::Error) -> Self {
        ProxyError::Io(e)
    }
}

/// 解析済みリクエストヘッダ部。
struct RequestHead {
    request_line: String,
    headers: Vec<(String, String)>,
    content_length: usize,
    host: Option<String>,
}

fn read_head<R: BufRead>(r: &mut R) -> Result<RequestHead, ProxyError> {
    let mut request_line = String::new();
    r.read_line(&mut request_line)?;
    if request_line.trim().is_empty() {
        return Err(ProxyError::BadRequest("empty request line"));
    }
    let mut total = request_line.len();
    let mut headers = vec![];
    let mut content_length = 0usize;
    let mut host = None;
    loop {
        let mut line = String::new();
        r.read_line(&mut line)?;
        total += line.len();
        if total > MAX_HEADER_BYTES {
            return Err(ProxyError::TooLarge("header section exceeds limit"));
        }
        let t = line.trim_end();
        if t.is_empty() {
            break;
        }
        let Some((k, v)) = t.split_once(':') else {
            return Err(ProxyError::BadRequest("malformed header"));
        };
        let (k, v) = (k.trim().to_string(), v.trim().to_string());
        if k.eq_ignore_ascii_case("content-length") {
            content_length = v
                .parse()
                .map_err(|_| ProxyError::BadRequest("bad content-length"))?;
            if content_length > MAX_BODY_BYTES {
                return Err(ProxyError::TooLarge("body exceeds limit"));
            }
        }
        if k.eq_ignore_ascii_case("host") {
            host = Some(v.clone());
        }
        headers.push((k, v));
    }
    Ok(RequestHead {
        request_line: request_line.trim_end().to_string(),
        headers,
        content_length,
        host,
    })
}

/// 1接続分のリクエストを Dispatcher で解決した upstream へ中継する。
///
/// `client` は accept 済みの接続。`peer` はログ/`X-Forwarded-For` 用の
/// クライアントアドレス文字列。成功時は中継したレスポンスのステータス行を返す。
pub fn proxy_once<D: Dispatcher>(
    client: TcpStream,
    peer: &str,
    dispatcher: &D,
    upstream_timeout: Duration,
) -> Result<String, ProxyError> {
    client.set_read_timeout(Some(upstream_timeout))?;
    let mut reader = BufReader::new(client.try_clone()?);
    let head = read_head(&mut reader)?;
    let host = head
        .host
        .clone()
        .ok_or(ProxyError::BadRequest("missing Host header"))?;
    let upstream = dispatcher
        .resolve(&host)
        .ok_or_else(|| ProxyError::NoRoute(host.clone()))?;

    // ボディ読み取り(Content-Length分)。
    let mut body = vec![0u8; head.content_length];
    if head.content_length > 0 {
        reader.read_exact(&mut body)?;
    }

    let mut up = TcpStream::connect((upstream.host.as_str(), upstream.port))?;
    up.set_read_timeout(Some(upstream_timeout))?;
    write_upstream_request(&mut up, &head, &body, &upstream, &host, peer)?;

    // レスポンス素通し(Connection: close 前提でEOFまで)。
    let mut resp = Vec::new();
    let mut up_reader = BufReader::new(up);
    up_reader.read_to_end(&mut resp)?;
    let status_line = resp
        .split(|&b| b == b'\n')
        .next()
        .map(|l| String::from_utf8_lossy(l).trim_end().to_string())
        .unwrap_or_default();

    let mut client_w = client;
    client_w.write_all(&resp)?;
    client_w.flush()?;
    // レスポンス送信後、書き込み方向を明示的に FIN で閉じる。drop 任せだと
    // ピアがまだ half-open(クライアントが応答を read 中)のままソケットを
    // 閉じることになり、Windows では RST(WSAECONNRESET / os error 10054)に
    // なってクライアント側の read が ConnectionReset で失敗することがある
    // (16 並行接続で再現。`serves_concurrent_requests_across_worker_threads`
    // が負荷時に fail していた原因)。keep-alive 非対応で `Connection: close`
    // を強制しているため、ここで write 方向を閉じて問題ない。
    let _ = client_w.shutdown(std::net::Shutdown::Write);
    Ok(status_line)
}

fn write_upstream_request(
    up: &mut TcpStream,
    head: &RequestHead,
    body: &[u8],
    upstream: &UpstreamAddr,
    original_host: &str,
    peer: &str,
) -> Result<(), ProxyError> {
    let mut out = String::new();
    out.push_str(&head.request_line);
    out.push_str("\r\n");
    for (k, v) in &head.headers {
        if k.eq_ignore_ascii_case("host")
            || k.eq_ignore_ascii_case("connection")
            || k.eq_ignore_ascii_case("x-forwarded-host")
            || k.eq_ignore_ascii_case("x-forwarded-for")
        {
            continue;
        }
        out.push_str(k);
        out.push_str(": ");
        out.push_str(v);
        out.push_str("\r\n");
    }
    out.push_str(&format!("Host: {}:{}\r\n", upstream.host, upstream.port));
    out.push_str(&format!("X-Forwarded-Host: {original_host}\r\n"));
    out.push_str(&format!("X-Forwarded-For: {peer}\r\n"));
    out.push_str("Connection: close\r\n\r\n");
    up.write_all(out.as_bytes())?;
    up.write_all(body)?;
    up.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RuntimeProfile, Stack, StaticDispatcher};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::thread;

    /// 最小のupstream: 受けたリクエストをエコーするHTTPサーバ。リスナーが
    /// 閉じられるまで受け付け続け、各接続の I/O エラーでは panic しない
    /// (高負荷CIでクライアント側 RST が起きてもテストヘルパーが落ちないように)。
    fn spawn_echo_upstream() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
            while let Ok((mut s, _)) = listener.accept() {
                thread::spawn(move || {
                    let Ok(clone) = s.try_clone() else { return };
                    let mut r = BufReader::new(clone);
                    let Ok(head) = read_head(&mut r) else { return };
                    let mut body = vec![0u8; head.content_length];
                    if head.content_length > 0 && r.read_exact(&mut body).is_err() {
                        return;
                    }
                    let echoed_host = head.host.unwrap_or_default();
                    let fwd_host = head
                        .headers
                        .iter()
                        .find(|(k, _)| k.eq_ignore_ascii_case("x-forwarded-host"))
                        .map(|(_, v)| v.clone())
                        .unwrap_or_default();
                    let payload = format!(
                        "upstream-host={echoed_host};fwd={fwd_host};body={}",
                        String::from_utf8_lossy(&body)
                    );
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                        payload.len()
                    );
                    let _ = s.write_all(resp.as_bytes());
                });
            }
        });
        port
    }

    #[test]
    fn proxies_request_to_resolved_upstream_and_returns_response() {
        let up_port = spawn_echo_upstream();
        let mut prof = RuntimeProfile::template(Stack::RustPoem, "t", "/tmp", up_port);
        prof.port = up_port;
        let mut d = StaticDispatcher::new();
        d.register("shop.example.jp", &prof);
        let d = Arc::new(d);

        // クライアント側リスナーを立て、接続ごとに proxy_once を回す
        // (閉じられるまでループ、エラーは無視)。
        let front = TcpListener::bind("127.0.0.1:0").unwrap();
        let fport = front.local_addr().unwrap().port();
        {
            let d = d.clone();
            thread::spawn(move || {
                while let Ok((c, addr)) = front.accept() {
                    let d = d.clone();
                    thread::spawn(move || {
                        let _ =
                            proxy_once(c, &addr.to_string(), d.as_ref(), Duration::from_secs(15));
                    });
                }
            });
        }

        // 1リクエスト=1接続。高負荷CIでの一過性の接続エラーは数回まで再試行
        // (検証したいのは Host 書き換え / X-Forwarded-Host / ボディ中継であって、
        // OS レベルの取りこぼしゼロではない)。
        let body = "hello=world";
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let mut resp = String::new();
        loop {
            resp.clear();
            let attempt = (|| -> std::io::Result<()> {
                let mut c = TcpStream::connect(("127.0.0.1", fport))?;
                c.set_read_timeout(Some(Duration::from_secs(10)))?;
                write!(
                    c,
                    "POST /buy HTTP/1.1\r\nHost: shop.example.jp\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                )?;
                c.flush()?;
                BufReader::new(&c).read_to_string(&mut resp)?;
                Ok(())
            })();
            if attempt.is_ok() && resp.starts_with("HTTP/1.1 200 OK") {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "proxied request never succeeded within 20s; last: {resp:?} err: {attempt:?}"
            );
            thread::sleep(Duration::from_millis(50));
        }

        assert!(
            resp.contains("fwd=shop.example.jp"),
            "X-Forwarded-Host must carry original host: {resp}"
        );
        assert!(
            resp.contains("body=hello=world"),
            "body must be relayed: {resp}"
        );
        assert!(
            resp.contains(&format!("upstream-host=127.0.0.1:{up_port}")),
            "Host must be rewritten to upstream: {resp}"
        );
    }

    #[test]
    fn unknown_host_yields_no_route() {
        let d = StaticDispatcher::new();
        let front = TcpListener::bind("127.0.0.1:0").unwrap();
        let fport = front.local_addr().unwrap().port();
        let h = thread::spawn(move || {
            let (c, addr) = front.accept().unwrap();
            proxy_once(c, &addr.to_string(), &d, Duration::from_secs(2))
        });
        let mut c = TcpStream::connect(("127.0.0.1", fport)).unwrap();
        write!(c, "GET / HTTP/1.1\r\nHost: nobody.example\r\n\r\n").unwrap();
        match h.join().unwrap() {
            Err(ProxyError::NoRoute(hst)) => assert_eq!(hst, "nobody.example"),
            other => panic!("expected NoRoute, got {other:?}"),
        }
    }
}
