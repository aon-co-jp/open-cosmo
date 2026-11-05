//! dummy_http_server — Supervisor(`open-runo-appserver`)の検証用ダミー
//! アプリケーションサーバー。標準ライブラリの`std::net`のみで実装した
//! 最小限のHTTP/1.1サーバーで、新規crate依存を追加しない
//! (このリポジトリの既存方針「軽量なテスト用実装は新規依存を足さない」
//! に従う)。
//!
//! 起動方法: `PORT=<port> cargo run -p open-runo-appserver --example
//! dummy_http_server`(`RuntimeProfile::build_command`が`PORT`環境変数を
//! 自動的に設定するため、Supervisor経由で起動する場合は明示不要)。
//!
//! 提供するエンドポイント:
//! - `GET /` または `GET /health` — `200 OK`(死活監視・実際に生きている
//!   ことの確認用)。
//! - `GET /crash` — このプロセス自身を`std::process::exit(1)`で
//!   異常終了させる(Supervisorのクラッシュ検知・自動再起動を検証する
//!   ためのトリガー)。レスポンスは返さず接続を切ってプロセスを
//!   終了する — クラッシュを模擬するため、正常なHTTPレスポンスは
//!   意図的に書かない。

use std::io::{BufRead, BufReader};
use std::net::{TcpListener, TcpStream};
use std::process;

fn main() {
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let listener = TcpListener::bind(("127.0.0.1", port)).expect("bind failed");
    // 実際にbindされたポートを標準出力へ(port=0で起動した場合、
    // 呼び出し側がOSに割り当てられたポートを知るため)。
    let actual_port = listener.local_addr().expect("local_addr").port();
    println!("dummy_http_server listening on 127.0.0.1:{actual_port}");

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => handle_connection(stream),
            Err(_) => continue,
        }
    }
}

fn handle_connection(mut stream: TcpStream) {
    let request_line = {
        let mut reader = BufReader::new(&mut stream);
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() {
            return;
        }
        line
    };

    // リクエストライン("GET /path HTTP/1.1")からパスだけを取り出す。
    let path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .to_string();

    if path == "/crash" {
        // 意図的な異常終了 — Supervisorのクラッシュ検知・自動再起動を
        // 検証するためのトリガー。正常なレスポンスは返さない。
        eprintln!("dummy_http_server: received /crash, exiting(1) to simulate a crash");
        process::exit(1);
    }

    use std::io::Write;
    let body = "ok";
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(response.as_bytes());
}
