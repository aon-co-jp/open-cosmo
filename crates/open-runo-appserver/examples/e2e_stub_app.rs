//! open-web-server <-> RPoem 実接続E2E検証用のスタブアプリ。
//!
//! `open-web-server` の `TenantRegistry` に `backend_addr` として登録される
//! 想定の、RPoem側 `ThreadedProxyServer`(`tenant_bridge::dispatcher_from_tenants`
//! 経由)を実際に1つ起動する。内部の upstream は "RPoemが処理した" ことが
//! 分かる固定レスポンスを返すだけの最小TCPエコーサーバー。
//!
//! 使い方: `cargo run -p open-runo-appserver --example e2e_stub_app -- <port>`
//! 起動後、`127.0.0.1:<port>` へのHTTPリクエストは
//! `Dispatcher`(`tenant_bridge::dispatcher_from_tenants`で構築)経由で
//! upstream へ転送され、`X-Served-By: RPoem-appserver` ヘッダ付きの
//! レスポンスが返る。

use open_runo_appserver::server::{ServerConfig, ThreadedProxyServer};
use open_runo_appserver::tenant_bridge::dispatcher_from_tenants;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::Arc;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let front_port: u16 = args.get(1).map(|s| s.parse().unwrap()).unwrap_or(0);

    // upstream: RPoemが処理したことを示す固定レスポンスを返す最小サーバー。
    let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
    let upstream_port = upstream.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in upstream.incoming().flatten() {
            std::thread::spawn(move || {
                let mut s = stream;
                let mut r = BufReader::new(s.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                        break;
                    }
                }
                let body = "hello from RPoem appserver";
                let _ = s.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nX-Served-By: RPoem-appserver\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
            });
        }
    });

    let addr = format!("127.0.0.1:{upstream_port}");
    let (dispatcher, rejected) = dispatcher_from_tenants([("e2e-rpoem.test", addr.as_str())]);
    assert!(rejected.is_empty(), "rejected tenants: {rejected:?}");

    let server = ThreadedProxyServer::start(
        &format!("127.0.0.1:{front_port}"),
        Arc::new(dispatcher),
        ServerConfig::default(),
    )
    .unwrap();

    // open-web-server 側から backend_addr として登録できるよう、
    // 実際にbindされたポートを標準出力へ出す(front_port=0のエフェメラル対応)。
    println!("RPOEM_APPSERVER_PORT={}", server.local_port);
    std::io::stdout().flush().unwrap();

    // プロセスは外部から終了させるまで待機し続ける。
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}
