//! world-lab-coordinator を RPoem(第二のTomcat)配下の管理対象プロセスと
//! して起動し、open-web-server(第二のApache)からリバースプロキシ経由で
//! 到達できるようにする統合例。
//!
//! ## 設計判断(2026-08-20、world-lab側からのユーザー指示への対応)
//!
//! world-labの独自実装 `world-lab-coordinator`(暗号化・リプレイ対策込みの
//! `dream-os-wire::WorkerChannel`/`CoordinatorChannel`)は貴重な既存資産
//! であり、作り直さない。かわりに、RPoemが既に持つ
//! `SupervisedTenantRegistry`(2026-08-06新設、`ProcessLifecycleManager`
//! による起動・ヘルスチェック監視・crash-loop backoff付き自動再起動)へ
//! `world-lab-coordinator`バイナリを**管理対象プロセスとして登録**し、
//! `ThreadedProxyServer`(マルチスレッド、既存)でリバースプロキシする。
//!
//! これにより:
//! - world-lab-coordinatorの暗号化・リプレイ対策ロジックには一切触れない
//!   (再利用、作り直しではない)。
//! - open-web-serverの`TenantRegistry`から見れば、このRPoemインスタンスは
//!   ただの`backend_addr`の1つであり、既存の`POST /admin/tenants`で
//!   何も新規コードを足さずに接続できる(2026-08-04に実証済みの
//!   tenant_bridge経路をそのまま再利用)。
//! - world-lab-coordinatorプロセスがクラッシュしても、RPoem側の監視が
//!   自動再起動する(Tomcat的なプロセスライフサイクル管理を、独立実装
//!   ではなくRPoemへ委ねる、というユーザー指示の核心)。
//!
//! ## 使い方
//!
//! ```text
//! cargo run --release -p open-runo-appserver --example world_lab_supervised -- \
//!   <world-lab-coordinator実行ファイルへのパス> <RPoem側プロキシのbindポート>
//! ```
//!
//! 起動後、標準出力に`WORLD_LAB_VIA_RPOEM_PORT=<port>`が出る。これを
//! open-web-server側の `POST /admin/tenants` の `backend_addr` として
//! 登録すれば、`Host: world-lab.internal`宛のリクエストが
//! open-web-server → RPoem(このプロセス) → world-lab-coordinator
//! という経路で到達する。

use open_runo_appserver::process_lifecycle::HealthState;
use open_runo_appserver::server::{ServerConfig, ThreadedProxyServer};
use open_runo_appserver::tenant_bridge::SupervisedTenantRegistry;
use open_runo_appserver::{RestartPolicy, RuntimeProfile, Stack};
use std::io::Write;
use std::time::Duration;

const TENANT_HOST: &str = "world-lab.internal";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let coordinator_bin = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "world-lab-coordinator".to_string());
    let proxy_bind_port: u16 = args.get(2).map(|s| s.parse().unwrap()).unwrap_or(0);
    // world-lab-coordinator自体は WORLD_LAB_BIND 環境変数でリッスン先を
    // 決める(既定 127.0.0.1:8300)。管理対象プロセスとして起動する側
    // (=RuntimeProfile)からもこのポートを明示指定して両者を一致させる。
    let coordinator_port: u16 = 18300;

    let mut profile = RuntimeProfile::template(
        Stack::Custom("world-lab-coordinator".to_string()),
        "world-lab-coordinator",
        ".",
        coordinator_port,
    );
    profile.command = coordinator_bin;
    profile.args = vec![];
    profile.env.insert(
        "WORLD_LAB_BIND".to_string(),
        format!("127.0.0.1:{coordinator_port}"),
    );
    // world-lab-coordinator の /health は既存実装通り GET /health。
    profile.health_path = Some("/health".to_string());

    let registry = SupervisedTenantRegistry::new();
    registry.register_with_managed_process(
        TENANT_HOST,
        profile,
        RestartPolicy::default(),
        Duration::from_millis(500),
    );

    // 起動直後にヘルスチェックが Healthy に到達するまで待つ(最大10秒)。
    // world-lab-coordinatorの起動自体は速いが、監視スレッドのポーリング
    // 間隔(500ms)との兼ね合いで即座には反映されないため。
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if matches!(
            registry.process_status(TENANT_HOST),
            Some(HealthState::Healthy)
        ) {
            break;
        }
        if std::time::Instant::now() > deadline {
            eprintln!(
                "warning: world-lab-coordinator did not report healthy within 10s (status={:?}) — proceeding anyway",
                registry.process_status(TENANT_HOST)
            );
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    let dispatcher = registry.dispatcher();
    let server = ThreadedProxyServer::start(
        &format!("127.0.0.1:{proxy_bind_port}"),
        dispatcher,
        ServerConfig::default(),
    )
    .expect("failed to start RPoem reverse-proxy front for world-lab-coordinator");

    println!("WORLD_LAB_VIA_RPOEM_PORT={}", server.local_port);
    println!("WORLD_LAB_TENANT_HOST={TENANT_HOST}");
    std::io::stdout().flush().unwrap();

    // 外部から明示的に終了させるまで待機し続ける。プロセス終了時、
    // SupervisedTenantRegistry(managedのArc)がdropされ、
    // ProcessLifecycleManagerのDrop実装が監視スレッドを終了させ
    // world-lab-coordinator子プロセスもkillする。
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}
