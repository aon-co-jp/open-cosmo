//! dummy_stubborn_server — `dummy_http_server.rs`と同じ最小HTTPサーバーだが、
//! **SIGTERMを明示的に無視する**(`Supervisor::stop_graceful`の
//! 「グレースフル終了に応じない=タイムアウト後SIGKILLへフォールバックする」
//! 経路を実証するための検証専用バイナリ)。Unix専用
//! (`signal(2)`のFFI宣言はUnix以外では意味を持たないため`main`自体を
//! `#[cfg(unix)]`限定にする——Windowsには任意プロセスへの
//! ポータブルなSIGTERM相当が無いという既知の制約と整合させるため)。
//!
//! 新規crate依存を追加しない(このリポジトリの既存方針)——`signal(2)`を
//! 自前で`extern "C"`宣言してSIG_IGNを設定する。

#[cfg(unix)]
fn main() {
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};

    extern "C" {
        fn signal(signum: i32, handler: usize) -> usize;
    }
    const SIGTERM: i32 = 15;
    const SIG_IGN: usize = 1;
    unsafe {
        signal(SIGTERM, SIG_IGN);
    }

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let listener = TcpListener::bind(("127.0.0.1", port)).expect("bind failed");
    let actual_port = listener.local_addr().expect("local_addr").port();
    println!("dummy_stubborn_server (ignores SIGTERM) listening on 127.0.0.1:{actual_port}");

    fn handle_connection(mut stream: TcpStream) {
        let mut reader = BufReader::new(&mut stream);
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() {
            return;
        }
        let body = "ok";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = stream.write_all(response.as_bytes());
    }

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => handle_connection(stream),
            Err(_) => continue,
        }
    }
}

#[cfg(not(unix))]
fn main() {
    eprintln!("dummy_stubborn_server is Unix-only (SIGTERM has no portable Windows equivalent)");
    std::process::exit(1);
}
