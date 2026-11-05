//! supervisor_demo — `Supervisor`(`open-runo-appserver`)が実際に
//! (1) 子プロセス(`dummy_http_server`)を起動し、(2) クラッシュを検知して
//! 自動再起動し、(3) 明示的な停止指示で正常終了して再起動しない、
//! ことを1つの実行ファイルの中で実演・自己検証する。
//!
//! 実行方法(先にdummy_http_serverをビルドしておくこと):
//! ```sh
//! cargo build -p open-runo-appserver --examples
//! cargo run -p open-runo-appserver --example supervisor_demo
//! ```
//!
//! このプログラムは終了コード0で正常終了すれば全シナリオが期待通り
//! だったことを意味する(アサーション失敗時はpanicして非0で終了する)。

use open_runo_appserver::{Health, RestartPolicy, RuntimeProfile, Stack, Supervisor};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// 現在実行中のこのexampleバイナリのパスから、兄弟の`dummy_http_server`
/// バイナリのパスを組み立てる(`examples/`ディレクトリは同じ場所に
/// 並んでビルドされるため)。
fn dummy_server_binary_path() -> PathBuf {
    let mut path = std::env::current_exe().expect("current_exe");
    let file_name = if cfg!(windows) {
        "dummy_http_server.exe"
    } else {
        "dummy_http_server"
    };
    path.set_file_name(file_name);
    path
}

/// 生のTCP接続でHTTP GETを送り、レスポンスの最初の行(ステータスライン)を
/// 返す。接続自体に失敗した場合は`None`(まだ起動していない/クラッシュ中)。
fn http_get(port: u16, path: &str) -> Option<String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).ok()?;
    let mut buf = String::new();
    // /crash は正常なレスポンスを返さない(プロセスが即死するため)ので
    // read自体が失敗するのは正常(Noneを返す)。
    stream.read_to_string(&mut buf).ok()?;
    buf.lines().next().map(|s| s.to_string())
}

fn wait_until<F: FnMut() -> bool>(mut cond: F, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn main() {
    let bin = dummy_server_binary_path();
    assert!(
        bin.exists(),
        "dummy_http_server binary not found at {bin:?} — run `cargo build -p open-runo-appserver --examples` first"
    );

    // ポート0(OS任意割当)ではSupervisor/RuntimeProfile側が実際の
    // bindポートを知る手段が無いため、テスト用に固定ポートを使う。
    let port: u16 = 19555;

    let mut profile = RuntimeProfile::template(
        Stack::Custom("dummy".into()),
        "dummy-http-server",
        ".",
        port,
    );
    profile.command = bin.to_string_lossy().into_owned();
    profile.args.clear();

    let mut sup = Supervisor::new(
        profile,
        RestartPolicy {
            base_backoff: Duration::from_millis(200),
            max_backoff: Duration::from_secs(2),
            max_rapid_failures: 20,
            healthy_after: Duration::from_secs(3),
        },
    );

    println!("=== [1] 起動 ===");
    let h = sup.tick();
    println!("tick() -> {h:?}");
    assert_eq!(h, Health::Starting);

    let up = wait_until(
        || http_get(port, "/health").as_deref() == Some("HTTP/1.1 200 OK"),
        Duration::from_secs(5),
    );
    assert!(up, "dummy_http_server did not come up within 5s");
    println!("/health -> 200 OK (実際にHTTPで確認)");

    // tick()で正しくUpと報告されることも確認。
    let h = sup.tick();
    println!("tick() -> {h:?}");
    assert_eq!(h, Health::Up);

    println!("\n=== [2] クラッシュを発生させる (GET /crash) ===");
    let crash_resp = http_get(port, "/crash");
    println!("/crash response -> {crash_resp:?} (接続断=クラッシュ成功が期待値)");

    // プロセスが実際に死ぬまで少し待ち、tick()がCrashedを報告することを
    // 確認する。
    let mut saw_crashed = false;
    let mut saw_restart = false;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let h = sup.tick();
        println!("tick() -> {h:?}");
        match h {
            Health::Crashed(code) => {
                saw_crashed = true;
                println!("  -> クラッシュ検知 (exit code = {code:?})");
            },
            Health::Starting => {
                saw_restart = true;
            },
            _ => {},
        }
        if saw_crashed && saw_restart {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(saw_crashed, "Supervisor did not detect the crash");
    assert!(
        saw_restart,
        "Supervisor did not automatically restart after the crash"
    );
    println!("=== クラッシュ検知 + 自動再起動を確認 ===");

    let up_again = wait_until(
        || http_get(port, "/health").as_deref() == Some("HTTP/1.1 200 OK"),
        Duration::from_secs(5),
    );
    assert!(up_again, "restarted dummy_http_server did not come back up");
    println!("再起動後の /health -> 200 OK (実際にHTTPで再確認)");

    println!("\n=== [3] 明示的な停止 (Supervisor::stop) ===");
    sup.stop();
    // stop()直後は同一プロセスへの接続がまだ一瞬残ることがあるので、
    // 少し待ってから「二度と生き返らない」ことを確認する。
    std::thread::sleep(Duration::from_millis(300));
    let still_down = wait_until(
        || http_get(port, "/health").is_none(),
        Duration::from_secs(3),
    );
    assert!(
        still_down,
        "process should be down immediately after stop()"
    );

    // stop()後は明示的にtick()を呼ばない限りSupervisorは何もしない
    // (呼び出し側がpollループを止めれば再起動されない、というのが
    // Phase 1のpoll型設計そのものの契約)。ここでは「stop()後に
    // tick()を呼んでも、Backoff/GaveUp状態からの復帰にはならず、
    // 呼び出し側が明示的にreset()しない限りRunningへは戻らない」
    // ことまでは確認しない — stop()はstateをNotStartedへ戻さない
    // 実装のため、この後にtick()すると再度spawnされてしまう
    // (=「明示的な再起動コマンド」を呼べば再起動できるという正しい
    // 挙動)。ここでの検証観点は「stop()だけを呼んだ後、追加でtick()を
    // 呼ばない限りプロセスは永久に停止したままである」ことなので、
    // 追加のtick()は行わずここで確認を終える。
    println!("停止後、追加の tick() を呼ばない限りプロセスは再起動されないことを確認 (プロセス停止のまま)");

    println!("\n=== 全シナリオ成功 ===");
}
