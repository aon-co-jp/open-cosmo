//! open-runo-appserver — 汎用アプリケーションサーバ・ランタイムホスト
//! (「第二のTomcat」— ただしJavaに限定しない。HYBRID_NETWORK_ARCHITECTURE.md §0.9 参照)
//!
//! 3つの抽象で構成する:
//! - [`RuntimeProfile`] — 言語×フレームワークごとの起動・監視方法の宣言的定義
//! - [`ProcessSupervisor`] — 子プロセスの起動・監視・crash-loop backoff付き再起動
//! - [`Dispatcher`] — open-web-server の app_proxy から受けたリクエストを
//!   稼働中プロファイルの upstream へ振り分けるための抽象(トランスポート非依存)
//!
//! Phase 1 は同期 `std::process` ベース。Poem/4層トランスポート統合は
//! 後続フェーズで `Dispatcher` 実装として追加する(§0.9.3)。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// 対応スタック(§0.9.2 の優先順)。`Custom` で任意拡張可能。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stack {
    RustPoem,
    PythonFastapi,
    PhpLaravel,
    RubyRails,
    DartFlutter,
    Custom(String),
}

/// 言語×フレームワークごとの宣言的ランタイム定義。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeProfile {
    /// 一意名(テナント/サイト単位)。
    pub name: String,
    pub stack: Stack,
    /// 起動コマンド(argv[0])。
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// 作業ディレクトリ(アプリのルート)。
    pub workdir: String,
    /// upstream の待受ポート。Dispatcher がここへ中継する。
    pub port: u16,
    /// ヘルスチェック用パス(例: "/health")。None なら TCP 接続のみで判定(将来)。
    #[serde(default)]
    pub health_path: Option<String>,
}

impl RuntimeProfile {
    /// 代表的スタックの雛形を返す。`workdir` と `port` は呼び出し側で必ず上書きする。
    pub fn template(stack: Stack, name: &str, workdir: &str, port: u16) -> Self {
        let (command, args, health): (&str, Vec<String>, Option<&str>) = match &stack {
            Stack::RustPoem => ("./target/release/app", vec![], Some("/health")),
            Stack::PythonFastapi => (
                "uvicorn",
                vec![
                    "main:app".into(),
                    "--host".into(),
                    "127.0.0.1".into(),
                    "--port".into(),
                    port.to_string(),
                ],
                Some("/docs"),
            ),
            Stack::PhpLaravel => (
                // 本番は open-easyweb-server の PHP-FPM 自動設定と連携する(§0.9.2 注記)。
                // 雛形は開発サーバ。
                "php",
                vec![
                    "artisan".into(),
                    "serve".into(),
                    "--host=127.0.0.1".into(),
                    format!("--port={port}"),
                ],
                Some("/"),
            ),
            Stack::RubyRails => (
                "bundle",
                vec![
                    "exec".into(),
                    "puma".into(),
                    "-b".into(),
                    format!("tcp://127.0.0.1:{port}"),
                ],
                Some("/up"),
            ),
            Stack::DartFlutter => (
                "dart",
                vec!["run".into(), "bin/server.dart".into()],
                Some("/"),
            ),
            Stack::Custom(_) => ("sh", vec![], None),
        };
        Self {
            name: name.to_string(),
            stack,
            command: command.to_string(),
            args,
            env: HashMap::new(),
            workdir: workdir.to_string(),
            port,
            health_path: health.map(str::to_string),
        }
    }

    fn build_command(&self) -> Command {
        let mut c = Command::new(&self.command);
        c.args(&self.args)
            .current_dir(&self.workdir)
            .envs(&self.env)
            .env("PORT", self.port.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        c
    }
}

/// crash-loop backoff の方針。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestartPolicy {
    /// 初回再起動までの待ち時間。
    pub base_backoff: Duration,
    /// backoff の上限。
    pub max_backoff: Duration,
    /// この回数連続で即死(下記 `healthy_after` 未満の生存)したら諦める。
    pub max_rapid_failures: u32,
    /// この時間生存したら「正常起動した」とみなし failure カウントをリセット。
    pub healthy_after: Duration,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self {
            base_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(30),
            max_rapid_failures: 5,
            healthy_after: Duration::from_secs(10),
        }
    }
}

/// 監視対象1プロセスの状態。
#[derive(Debug)]
enum ProcState {
    NotStarted,
    Running {
        child: Child,
        started: Instant,
        /// このプロセスに至るまでの連続即死回数(healthy_after 生存でリセット)。
        prior_failures: u32,
    },
    Backoff { until: Instant, failures: u32 },
    GaveUp { failures: u32 },
}

/// プロファイル1件を監視するスーパーバイザ。
///
/// Phase 1 は poll 型: 呼び出し側(常駐ループや Poem のバックグラウンドタスク)が
/// 定期的に [`Supervisor::tick`] を呼ぶ。スレッドやランタイムを内部で持たないので
/// 同期/非同期どちらの世界にも埋め込める。
pub struct Supervisor {
    pub profile: RuntimeProfile,
    pub policy: RestartPolicy,
    state: ProcState,
}

/// `tick` が報告する観測結果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Health {
    Starting,
    Up,
    /// exit code(取れた場合)。
    Crashed(Option<i32>),
    BackingOff,
    GaveUp,
}

impl Supervisor {
    pub fn new(profile: RuntimeProfile, policy: RestartPolicy) -> Self {
        Self {
            profile,
            policy,
            state: ProcState::NotStarted,
        }
    }

    /// 1回分の監視処理。必要なら起動/再起動する。
    pub fn tick(&mut self) -> Health {
        let now = Instant::now();
        match &mut self.state {
            ProcState::NotStarted => self.spawn(0),
            ProcState::Running {
                child,
                started,
                prior_failures,
            } => match child.try_wait() {
                Ok(None) => Health::Up,
                Ok(Some(status)) => {
                    let lived = now.duration_since(*started);
                    // healthy_after 以上生きたなら「1回目の失敗」から数え直し、
                    // 即死なら連続失敗としてカウントを積む。
                    let failures = if lived >= self.policy.healthy_after {
                        1
                    } else {
                        prior_failures.saturating_add(1)
                    };
                    let code = status.code();
                    self.enter_backoff(failures, now);
                    Health::Crashed(code)
                }
                Err(_) => {
                    let failures = prior_failures.saturating_add(1);
                    self.enter_backoff(failures, now);
                    Health::Crashed(None)
                }
            },
            ProcState::Backoff { until, failures } => {
                if *failures >= self.policy.max_rapid_failures {
                    let f = *failures;
                    self.state = ProcState::GaveUp { failures: f };
                    Health::GaveUp
                } else if now >= *until {
                    let f = *failures;
                    self.spawn(f)
                } else {
                    Health::BackingOff
                }
            }
            ProcState::GaveUp { .. } => Health::GaveUp,
        }
    }

    /// 手動リセット(設定修正後の再挑戦用)。
    pub fn reset(&mut self) {
        self.stop();
        self.state = ProcState::NotStarted;
    }

    /// 稼働中なら kill する。
    pub fn stop(&mut self) {
        if let ProcState::Running { child, .. } = &mut self.state {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn spawn(&mut self, prior_failures: u32) -> Health {
        match self.profile.build_command().spawn() {
            Ok(child) => {
                self.state = ProcState::Running {
                    child,
                    started: Instant::now(),
                    prior_failures,
                };
                Health::Starting
            }
            Err(_) => {
                self.enter_backoff(prior_failures + 1, Instant::now());
                Health::Crashed(None)
            }
        }
    }

    fn enter_backoff(&mut self, failures: u32, now: Instant) {
        let exp = failures.saturating_sub(1).min(16);
        let backoff = self
            .policy
            .base_backoff
            .saturating_mul(1u32 << exp)
            .min(self.policy.max_backoff);
        self.state = ProcState::Backoff {
            until: now + backoff,
            failures,
        };
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.stop();
    }
}

/// リクエスト中継の抽象。Phase 1 では「どの upstream へ送るべきか」の解決のみを
/// 提供し、実際のプロキシ実装(Poem ハンドラ、4層トランスポート)は後続フェーズで
/// この trait を実装する。
pub trait Dispatcher {
    /// ホスト名(テナントドメイン)から upstream アドレスを解決する。
    fn resolve(&self, host: &str) -> Option<UpstreamAddr>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpstreamAddr {
    pub host: String,
    pub port: u16,
}

/// ホスト名 → プロファイルの静的対応表による最小 Dispatcher 実装。
/// open-web-server の TenantRegistry と接続する際は、この表を registry 由来で
/// 構築するアダプタを書けばよい。
#[derive(Default)]
pub struct StaticDispatcher {
    routes: HashMap<String, UpstreamAddr>,
}

impl StaticDispatcher {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn register(&mut self, host: &str, profile: &RuntimeProfile) {
        self.routes.insert(
            host.to_ascii_lowercase(),
            UpstreamAddr {
                host: "127.0.0.1".into(),
                port: profile.port,
            },
        );
    }
}

impl Dispatcher for StaticDispatcher {
    fn resolve(&self, host: &str) -> Option<UpstreamAddr> {
        // ポート付き Host ヘッダ("example.com:8080")も許容する。
        let h = host.split(':').next().unwrap_or(host).to_ascii_lowercase();
        self.routes.get(&h).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_cover_priority_matrix() {
        for stack in [
            Stack::RustPoem,
            Stack::PythonFastapi,
            Stack::PhpLaravel,
            Stack::RubyRails,
            Stack::DartFlutter,
        ] {
            let p = RuntimeProfile::template(stack.clone(), "t", "/tmp", 9001);
            assert_eq!(p.port, 9001);
            assert!(!p.command.is_empty(), "{stack:?} has a command");
        }
    }

    #[test]
    fn profile_roundtrips_through_json() {
        let p = RuntimeProfile::template(Stack::PythonFastapi, "api", "/srv/api", 8000);
        let s = serde_json::to_string(&p).unwrap();
        let q: RuntimeProfile = serde_json::from_str(&s).unwrap();
        assert_eq!(q.name, "api");
        assert_eq!(q.stack, Stack::PythonFastapi);
    }

    #[test]
    fn supervisor_restarts_short_lived_process_with_backoff_then_gives_up() {
        // "true" は即終了するので crash-loop 相当になる。
        let mut prof = RuntimeProfile::template(Stack::Custom("noop".into()), "n", "/tmp", 1);
        prof.command = "true".into();
        prof.args.clear();
        let mut sup = Supervisor::new(
            prof,
            RestartPolicy {
                base_backoff: Duration::from_millis(1),
                max_backoff: Duration::from_millis(2),
                max_rapid_failures: 3,
                healthy_after: Duration::from_secs(60),
            },
        );
        let mut saw_gave_up = false;
        for _ in 0..200 {
            match sup.tick() {
                Health::GaveUp => {
                    saw_gave_up = true;
                    break;
                }
                Health::BackingOff => std::thread::sleep(Duration::from_millis(2)),
                _ => {}
            }
        }
        assert!(saw_gave_up, "crash-looping process must eventually give up");
    }

    #[test]
    fn supervisor_reports_up_for_long_running_process_and_stops_it() {
        let mut prof = RuntimeProfile::template(Stack::Custom("sleep".into()), "s", "/tmp", 1);
        prof.command = "sleep".into();
        prof.args = vec!["30".into()];
        let mut sup = Supervisor::new(prof, RestartPolicy::default());
        assert_eq!(sup.tick(), Health::Starting);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(sup.tick(), Health::Up);
        sup.stop();
    }

    #[test]
    fn static_dispatcher_resolves_host_with_and_without_port() {
        let p = RuntimeProfile::template(Stack::RustPoem, "shop", "/srv/shop", 4100);
        let mut d = StaticDispatcher::new();
        d.register("Shop.Example.JP", &p);
        assert_eq!(d.resolve("shop.example.jp").unwrap().port, 4100);
        assert_eq!(d.resolve("shop.example.jp:443").unwrap().port, 4100);
        assert!(d.resolve("other.example.jp").is_none());
    }
}
