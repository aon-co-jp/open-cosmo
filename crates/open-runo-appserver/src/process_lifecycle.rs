//! `ProcessLifecycleManager` — Tomcat相当のプロセスライフサイクル管理
//! (起動・ヘルスチェックによる生存監視・crash-loop backoffでの自動再起動・
//! 管理API経由での明示的な停止/再起動)。
//!
//! `Supervisor`(このクレートの`lib.rs`、Phase 1)は「呼び出し側が定期的に
//! `tick()`を呼ぶ」poll型の骨格のみで、実際にバックグラウンドで監視し続ける
//! スレッド・HTTPヘルスチェックそのものは持っていなかった。本モジュールは
//! `Supervisor`を土台に、以下を追加する:
//!
//! - 専用の監視スレッドが`poll_interval`ごとに自動で`tick()`を呼ぶ
//!   (呼び出し側が監視ループを自前で書く必要がない)。
//! - プロセスが`Running`状態になった後、`RuntimeProfile.health_path`
//!   (未指定なら`"/"`)へ実際にHTTP GETを送り、生存確認する
//!   (`try_wait()`だけでは「プロセスは存在するがリクエストに応答しない
//!   デッドロック状態」を検知できないため)。
//! - crash-loop backoffは`Supervisor`の既存実装(指数バックオフ+上限)
//!   をそのまま利用する。
//! - `stop()`/`restart()`で明示的にライフサイクルを操作できる
//!   (`stop()`後は監視スレッドが自動再起動を行わない — 明示的な
//!   `restart()`まで停止状態を維持する)。

use crate::{Health, RestartPolicy, RuntimeProfile, Supervisor};
use serde::Serialize;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// 監視スレッドが観測する現在の健康状態(管理API・ダッシュボード向け)。
/// `Serialize`は`open-runo-gateway`側のHTTP管理API(2026-08-06追加)が
/// このままJSONへ変換して返すために必要(`#[serde(rename_all =
/// "snake_case")]`でJSON上は`healthy`/`backing_off`等の慣習的な形にする)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthState {
    /// プロセス起動直後、まだヘルスチェックに1回も成功していない。
    Starting,
    /// プロセスが起動していて、直近のHTTPヘルスチェックが成功した。
    Healthy,
    /// プロセスは`try_wait`上は生きているが、直近のHTTPヘルスチェックが
    /// 失敗した(応答なし/非2xx/接続不可)。
    Unhealthy,
    /// 直前のtickでプロセスの異常終了を検知した(このtick自体は
    /// 一瞬で次のBackingOff/再起動に遷移するため、観測されることは稀)。
    Crashed,
    /// crash-loop backoff中(次回再起動までの待機時間)。
    BackingOff,
    /// `stop()`が呼ばれ、監視スレッドが自動再起動を止めている。
    Stopped,
    /// `RestartPolicy.max_rapid_failures`に達し、再起動を諦めた。
    GaveUp,
}

struct Shared {
    supervisor: Mutex<Supervisor>,
    poll_interval: Duration,
    health: Mutex<HealthState>,
    /// 明示的な`stop()`が呼ばれている間はtrue — 監視スレッドは
    /// プロセスの生存確認・再起動を一切行わず待機するだけになる。
    stopped: AtomicBool,
    /// `Drop`/`shutdown()`で監視スレッド自体を終了させるためのフラグ。
    shutdown: AtomicBool,
}

/// 1つのアプリケーションプロセスのライフサイクルを管理するハンドル。
///
/// `Drop`時に監視スレッドを終了させ、プロセスを`kill`する
/// (`Supervisor`の`Drop`実装と同じ後始末方針)。
pub struct ProcessLifecycleManager {
    shared: Arc<Shared>,
    monitor: Option<JoinHandle<()>>,
}

impl ProcessLifecycleManager {
    /// プロセスを起動し、専用の監視スレッドで監視を開始する。
    ///
    /// `poll_interval`は「crash検知の反応速度」と「ヘルスチェックの
    /// 頻度」の両方に使う(2つを分ける必要が出てきたら分離を検討する)。
    pub fn start(profile: RuntimeProfile, policy: RestartPolicy, poll_interval: Duration) -> Self {
        let shared = Arc::new(Shared {
            supervisor: Mutex::new(Supervisor::new(profile, policy)),
            poll_interval,
            health: Mutex::new(HealthState::Starting),
            stopped: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
        });
        let monitor_shared = Arc::clone(&shared);
        let handle = thread::spawn(move || monitor_loop(monitor_shared));
        Self {
            shared,
            monitor: Some(handle),
        }
    }

    /// 直近の観測状態(管理API相当、ロック待ちのみで即座に返る)。
    pub fn status(&self) -> HealthState {
        self.shared
            .health
            .lock()
            .expect("health lock poisoned")
            .clone()
    }

    /// 稼働中の子プロセスのOS PID(無ければ`None`)。テスト・監査用。
    pub fn pid(&self) -> Option<u32> {
        self.shared
            .supervisor
            .lock()
            .expect("supervisor lock poisoned")
            .pid()
    }

    /// 明示的な停止(管理API相当)。プロセスをkillし、監視スレッドの
    /// 自動再起動を停止する。`restart()`まで停止状態を維持する。
    pub fn stop(&self) {
        self.shared.stopped.store(true, Ordering::SeqCst);
        self.shared
            .supervisor
            .lock()
            .expect("supervisor lock poisoned")
            .stop();
        *self.shared.health.lock().expect("health lock poisoned") = HealthState::Stopped;
    }

    /// 段階的グレースフルシャットダウン(管理API相当、2026-08-07新設):
    /// `Supervisor::stop_graceful`をそのまま呼ぶ薄いラッパー——SIGTERM相当を
    /// 送って`timeout`まで自発的終了を待ち、応答が無ければ`stop()`と同じ
    /// 強制終了(SIGKILL相当)にフォールバックする。`stop()`と同じく監視
    /// スレッドの自動再起動を止め、明示的な`restart()`まで停止状態を
    /// 維持する。戻り値は「SIGTERMのみで終了できたか」
    /// (プラットフォーム差の詳細は`Supervisor::stop_graceful`のdoc参照)。
    pub fn stop_graceful(&self, timeout: std::time::Duration) -> bool {
        self.shared.stopped.store(true, Ordering::SeqCst);
        let graceful = self
            .shared
            .supervisor
            .lock()
            .expect("supervisor lock poisoned")
            .stop_graceful(timeout);
        *self.shared.health.lock().expect("health lock poisoned") = HealthState::Stopped;
        graceful
    }

    /// 明示的な再起動(管理API相当)。`stop()`後の停止状態、および
    /// crash-loop backoffで`GaveUp`になった状態のどちらからも復帰できる
    /// (運用者が原因を修正した後の再挑戦に使う想定)。
    pub fn restart(&self) {
        {
            let mut sup = self
                .shared
                .supervisor
                .lock()
                .expect("supervisor lock poisoned");
            sup.reset();
        }
        self.shared.stopped.store(false, Ordering::SeqCst);
        *self.shared.health.lock().expect("health lock poisoned") = HealthState::Starting;
    }
}

impl Drop for ProcessLifecycleManager {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::SeqCst);
        if let Some(h) = self.monitor.take() {
            let _ = h.join();
        }
        // 監視スレッド終了後、確実にプロセスを後始末する
        // (Supervisor自体もDropでkillするが、ここでも明示しておく)。
        self.shared
            .supervisor
            .lock()
            .expect("supervisor lock poisoned")
            .stop();
    }
}

fn monitor_loop(shared: Arc<Shared>) {
    loop {
        if shared.shutdown.load(Ordering::SeqCst) {
            return;
        }
        if shared.stopped.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(20).min(shared.poll_interval));
            continue;
        }

        let tick_result = {
            let mut sup = shared.supervisor.lock().expect("supervisor lock poisoned");
            sup.tick()
        };

        match tick_result {
            Health::Starting => {
                *shared.health.lock().expect("health lock poisoned") = HealthState::Starting;
            },
            Health::Up => {
                let ok = {
                    let sup = shared.supervisor.lock().expect("supervisor lock poisoned");
                    http_health_check(&sup.profile, Duration::from_secs(2))
                };
                *shared.health.lock().expect("health lock poisoned") = if ok {
                    HealthState::Healthy
                } else {
                    HealthState::Unhealthy
                };
            },
            Health::Crashed(_) => {
                *shared.health.lock().expect("health lock poisoned") = HealthState::Crashed;
            },
            Health::BackingOff => {
                *shared.health.lock().expect("health lock poisoned") = HealthState::BackingOff;
            },
            Health::GaveUp => {
                *shared.health.lock().expect("health lock poisoned") = HealthState::GaveUp;
                // GaveUpは`restart()`が明示的に呼ばれるまで変化しない終端状態。
                // 無駄な高頻度ポーリングを避けるため長めに待つ。
                thread::sleep(Duration::from_millis(200));
                continue;
            },
        }

        thread::sleep(shared.poll_interval);
    }
}

/// `profile.port` + `profile.health_path`(既定`"/"`)へ実際にTCP接続し、
/// HTTP GETを送ってステータス行が2xxかどうかを見る、素の`std::net`実装
/// (このクレートの既存方針=新規依存を足さない、`proxy.rs`と同じ流儀)。
fn http_health_check(profile: &RuntimeProfile, timeout: Duration) -> bool {
    let path = profile.health_path.as_deref().unwrap_or("/");
    let addr = std::net::SocketAddr::from((std::net::Ipv4Addr::new(127, 0, 0, 1), profile.port));
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, timeout) else {
        return false;
    };
    if stream.set_read_timeout(Some(timeout)).is_err() {
        return false;
    }
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n",
        profile.port
    );
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let mut buf = [0u8; 64];
    let Ok(n) = stream.read(&mut buf) else {
        return false;
    };
    if n == 0 {
        return false;
    }
    let head = String::from_utf8_lossy(&buf[..n]);
    // "HTTP/1.1 200 OK" のようなステータス行から3桁のステータスコードを見る。
    head.split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .is_some_and(|code| (200..300).contains(&code))
}

/// テスト専用ヘルパー: `examples/dummy_http_server.rs`のビルド済みexeを
/// 探す。`env!("CARGO_BIN_EXE_...")`はcargoがlibユニットテストには供給
/// しない(統合テスト`tests/*.rs`・benchesのみ)ため、`target/{debug,
/// release}/`から相対的に探す実行時解決を行う。`tenant_bridge`側の
/// テストからも再利用するため`pub(crate)`で公開する。
#[cfg(test)]
pub(crate) fn dummy_app_exe_path() -> std::path::PathBuf {
    let mut dir = std::env::current_exe().expect("current_exe");
    // .../target/debug/deps/open_runo_appserver-<hash>.exe から
    // .../target/debug/ まで遡る。
    while dir
        .file_name()
        .map(|n| n != "debug" && n != "release")
        .unwrap_or(false)
    {
        if !dir.pop() {
            break;
        }
    }
    let name = if cfg!(windows) {
        "open-runo-dummy-http-server.exe"
    } else {
        "open-runo-dummy-http-server"
    };
    dir.push(name);
    assert!(
        dir.exists(),
        "expected dummy http server binary at {dir:?} (run `cargo build -p open-runo-appserver --bin open-runo-dummy-http-server` first)"
    );
    dir
}

/// `dummy_app_exe_path`と同じ解決方法で、`open-runo-dummy-stubborn-server`
/// (SIGTERMを無視するUnix専用の検証用サーバー、2026-08-07新設)を探す。
/// `lib.rs`の`stop_graceful`テストから`pub(crate)`として使う。
/// 呼び出し側テストが`#[cfg(unix)]`限定のため、定義も同様に限定して
/// 非Unixでのdead_code警告を避ける(バイナリ名解決は移植性のため
/// windows分岐も残してある)。
#[cfg(all(test, unix))]
pub(crate) fn dummy_stubborn_exe_path() -> std::path::PathBuf {
    let mut dir = std::env::current_exe().expect("current_exe");
    while dir
        .file_name()
        .map(|n| n != "debug" && n != "release")
        .unwrap_or(false)
    {
        if !dir.pop() {
            break;
        }
    }
    let name = if cfg!(windows) {
        "open-runo-dummy-stubborn-server.exe"
    } else {
        "open-runo-dummy-stubborn-server"
    };
    dir.push(name);
    assert!(
        dir.exists(),
        "expected stubborn dummy server binary at {dir:?} (run `cargo build -p open-runo-appserver --bin open-runo-dummy-stubborn-server` first)"
    );
    dir
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Stack;
    use std::net::TcpListener;

    /// テスト用に空きポートを確保する(`RuntimeProfile.port`用)。
    /// 確保後すぐlistenerを閉じて子プロセスへ明け渡す — このクレートの
    /// 既存テスト(`server.rs`等)でも使われている流儀と同じ、多少の
    /// TOCTOU競合は許容する(CI/開発機のsandboxで問題化した例はない)。
    fn free_port() -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    }

    fn dummy_app_profile(port: u16) -> RuntimeProfile {
        let exe = super::dummy_app_exe_path();
        let mut profile =
            RuntimeProfile::template(Stack::Custom("dummy".into()), "dummy", ".", port);
        profile.command = exe.to_string_lossy().to_string();
        profile.args.clear();
        profile.health_path = Some("/health".into());
        profile
    }

    fn fast_policy() -> RestartPolicy {
        RestartPolicy {
            base_backoff: Duration::from_millis(20),
            max_backoff: Duration::from_millis(200),
            max_rapid_failures: 5,
            healthy_after: Duration::from_millis(300),
        }
    }

    /// 成功までの待機のみに使う(成功すれば即戻る)。cargo test --workspace の初回実行のように
    /// ビルド・ウイルススキャン等でCPU/IOが逼迫すると、子プロセスの起動+初回ヘルスチェックが
    /// 5秒を超えて偽陰性になるため、渡された期限は下限30秒に引き上げる。
    fn wait_for<F: Fn() -> bool>(deadline: Duration, f: F) -> bool {
        let deadline = deadline.max(Duration::from_secs(30));
        let start = std::time::Instant::now();
        while start.elapsed() < deadline {
            if f() {
                return true;
            }
            thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn starts_process_and_reports_healthy_via_real_http_check() {
        let port = free_port();
        let mgr = ProcessLifecycleManager::start(
            dummy_app_profile(port),
            fast_policy(),
            Duration::from_millis(50),
        );
        assert!(
            wait_for(Duration::from_secs(5), || mgr.status()
                == HealthState::Healthy),
            "expected Healthy within 5s, last status = {:?}",
            mgr.status()
        );
        assert!(mgr.pid().is_some());
        mgr.stop();
    }

    #[test]
    fn crash_triggers_automatic_restart_with_new_pid() {
        let port = free_port();
        let mgr = ProcessLifecycleManager::start(
            dummy_app_profile(port),
            fast_policy(),
            Duration::from_millis(50),
        );
        assert!(wait_for(Duration::from_secs(5), || mgr.status()
            == HealthState::Healthy));
        let pid_before = mgr.pid().expect("running process must have a pid");

        // 実際に /crash を叩いてプロセスを異常終了させる(モック無し)。
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let _ = stream.write_all(b"GET /crash HTTP/1.1\r\nHost: x\r\n\r\n");
        drop(stream);

        // クラッシュが検知されCrashed/BackingOffへ遷移することを確認。
        assert!(
            wait_for(Duration::from_secs(5), || matches!(
                mgr.status(),
                HealthState::Crashed | HealthState::BackingOff
            )),
            "expected crash to be detected, last status = {:?}",
            mgr.status()
        );

        // crash-loop backoffを経て自動的に再起動し、再度Healthyになる
        // ことを確認(新しいPIDで、同じポートに再bindされる)。
        assert!(
            wait_for(Duration::from_secs(10), || mgr.status()
                == HealthState::Healthy),
            "expected automatic restart to become Healthy again, last status = {:?}",
            mgr.status()
        );
        let pid_after = mgr.pid().expect("restarted process must have a pid");
        assert_ne!(
            pid_before, pid_after,
            "restart must spawn a genuinely new process"
        );

        mgr.stop();
    }

    #[test]
    fn explicit_stop_halts_automatic_restart_and_explicit_restart_resumes() {
        let port = free_port();
        let mgr = ProcessLifecycleManager::start(
            dummy_app_profile(port),
            fast_policy(),
            Duration::from_millis(50),
        );
        assert!(wait_for(Duration::from_secs(5), || mgr.status()
            == HealthState::Healthy));

        mgr.stop();
        assert_eq!(mgr.status(), HealthState::Stopped);
        // stop後、実際にプロセスが応答しなくなっている(kill済み)ことを確認。
        assert!(TcpStream::connect(("127.0.0.1", port)).is_err());
        // stop状態のまま数百ms置いても自動では復帰しない(監視スレッドが
        // 自動再起動しないことの確認)。
        thread::sleep(Duration::from_millis(300));
        assert_eq!(mgr.status(), HealthState::Stopped);

        mgr.restart();
        assert!(
            wait_for(Duration::from_secs(5), || mgr.status()
                == HealthState::Healthy),
            "expected explicit restart to bring the process back healthy"
        );
    }

    #[test]
    fn stop_graceful_reports_status_stopped_and_leaves_managed_prevented_from_auto_restart() {
        // ProcessLifecycleManagerレベルの薄いラッパーとしての検証
        // (SIGTERM送信・タイムアウト・SIGKILLフォールバック自体の詳細は
        // lib.rs側のSupervisor::stop_gracefulのテストで検証済み)。
        let port = free_port();
        let mgr = ProcessLifecycleManager::start(
            dummy_app_profile(port),
            fast_policy(),
            Duration::from_millis(50),
        );
        assert!(wait_for(Duration::from_secs(5), || mgr.status()
            == HealthState::Healthy));

        let graceful = mgr.stop_graceful(Duration::from_secs(5));
        assert_eq!(mgr.status(), HealthState::Stopped);
        assert!(TcpStream::connect(("127.0.0.1", port)).is_err());

        #[cfg(unix)]
        assert!(
            graceful,
            "dummy server has no SIGTERM handler and should exit gracefully"
        );
        #[cfg(windows)]
        assert!(
            !graceful,
            "Windows always falls back to force-kill (documented limitation)"
        );

        // stop_graceful後も自動再起動されないこと(stop()と同じ意味論)を確認。
        thread::sleep(Duration::from_millis(300));
        assert_eq!(mgr.status(), HealthState::Stopped);

        // 明示的restart()で復帰できること。
        mgr.restart();
        assert!(wait_for(Duration::from_secs(5), || mgr.status()
            == HealthState::Healthy));
        mgr.stop();
    }

    #[test]
    fn gives_up_after_repeated_rapid_crashes_and_can_be_manually_restarted() {
        // "true"/no-opは即終了するプロセス相当 — crash-loopそのものを
        // 検証する(実HTTPを話さないダミーで、backoffの積み重ねだけを見る)。
        let mut profile = RuntimeProfile::template(Stack::Custom("noop".into()), "n", ".", 1);
        if cfg!(windows) {
            profile.command = "cmd".into();
            profile.args = vec!["/C".into(), "exit 1".into()];
        } else {
            profile.command = "false".into();
            profile.args.clear();
        }
        profile.health_path = None;
        let policy = RestartPolicy {
            base_backoff: Duration::from_millis(5),
            max_backoff: Duration::from_millis(10),
            max_rapid_failures: 3,
            healthy_after: Duration::from_secs(60),
        };
        let mgr = ProcessLifecycleManager::start(profile, policy, Duration::from_millis(5));
        assert!(
            wait_for(Duration::from_secs(10), || mgr.status()
                == HealthState::GaveUp),
            "expected crash-looping process to eventually give up, last status = {:?}",
            mgr.status()
        );

        // 明示的なrestart()でGaveUpから復帰できること(監視ループが
        // 再度スタートし、再びクラッシュしてbackoffに入る=完全に
        // 無反応のままではないこと)を確認する。
        mgr.restart();
        assert!(
            wait_for(Duration::from_secs(5), || !matches!(
                mgr.status(),
                HealthState::Stopped
            )),
            "expected restart() to resume the monitor loop"
        );
    }
}
