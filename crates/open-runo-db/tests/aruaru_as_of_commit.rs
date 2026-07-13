//! Real end-to-end proof of the commit-ID read-side query API
//! (`DbBackend::get_at_commit`, `AruaruDbBackend`'s implementation).
//!
//! Spawns the actual `aruaru-server` binary (built from the sibling
//! `aruaru-db` repository at `F:\open-runo\aruaru-db`) as a child process,
//! connects to its real pgwire endpoint with `sqlx` (the same client
//! `AruaruDbBackend` uses in production), performs two real writes with an
//! explicit `aruaru_commit()` between them, and asserts that querying the
//! *first* commit ID returns the *old* value, not the latest one.
//!
//! `#[ignore]`d by default (like this workspace's other cross-process/
//! cross-repo integration tests) because it depends on a sibling repo's
//! build artifact existing at a fixed relative path — not something CI or
//! a fresh checkout can assume. Run explicitly with:
//! `cargo test -p open-runo-db --features aruaru --test aruaru_as_of_commit -- --ignored --nocapture`

#![cfg(feature = "aruaru")]

use open_runo_db::{migration, DbBackend};
use sqlx::PgPool;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

struct ServerGuard(Child);
impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn aruaru_server_binary() -> Option<PathBuf> {
    // F:\open-runo\open-runo\crates\open-runo-db -> F:\open-runo
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repos_root = manifest_dir.parent()?.parent()?.parent()?;
    let candidate = repos_root
        .join("aruaru-db")
        .join("target")
        .join("debug")
        .join(if cfg!(windows) { "aruaru-server.exe" } else { "aruaru-server" });
    candidate.exists().then_some(candidate)
}

#[tokio::test]
#[ignore = "spawns the real aruaru-server binary from the sibling aruaru-db repo; run explicitly"]
async fn as_of_commit_returns_the_old_value_through_the_real_pgwire_endpoint() {
    let binary = aruaru_server_binary().expect(
        "aruaru-server binary not found — build it first: \
         `cd ../aruaru-db && cargo build -p aruaru-server`",
    );

    let port = 15433u16;
    let data_dir = std::env::temp_dir().join(format!("aruaru-as-of-commit-test-{}", std::process::id()));
    std::fs::create_dir_all(&data_dir).expect("create temp data dir");

    let child = Command::new(&binary)
        .arg("--pg-port").arg(port.to_string())
        .arg("--gql-port").arg("0")
        .arg("--data").arg(&data_dir)
        .arg("--log-level").arg("warn")
        .env("ARUARU_USERS", "aruaru:aruaru")
        .spawn()
        .expect("spawn aruaru-server");
    let _guard = ServerGuard(child);

    let url = format!("postgres://aruaru:aruaru@127.0.0.1:{port}/aruaru");
    let pool = connect_with_retry(&url).await;

    // Schema + backend under test, exactly as open-runo-router would use it.
    sqlx::query(migration::KV_STORE_DDL_ARUARU)
        .execute(&pool)
        .await
        .expect("create kv_store");
    let backend = open_runo_db::aruaru::AruaruDbBackend::from_pool(pool.clone());

    // 1st write + commit.
    backend.put("items", "sword", r#"{"qty":1}"#).await.expect("put 1");
    let commit_1 = aruaru_commit(&pool, "first grant").await;

    // 2nd write (same key, new value) + commit.
    backend.put("items", "sword", r#"{"qty":5}"#).await.expect("put 2");
    let _commit_2 = aruaru_commit(&pool, "quantity bumped").await;

    // Latest value is qty=5.
    let latest = backend.get("items", "sword").await.expect("get").expect("value present");
    assert!(latest.contains("\"qty\":5"), "latest value should be qty=5, got {latest}");

    // AS OF the first commit must return qty=1 — the old value, not the latest.
    let historical = backend
        .get_at_commit("items", "sword", &commit_1)
        .await
        .expect("get_at_commit should succeed")
        .expect("value should have existed at commit_1");
    assert!(
        historical.contains("\"qty\":1"),
        "AS OF COMMIT '{commit_1}' should return the OLD value (qty=1), got: {historical}"
    );
    assert_ne!(historical, latest, "AS OF COMMIT must not silently return the latest value");

    // An unknown commit id must not be confused with "key never existed".
    let unknown = backend.get_at_commit("items", "sword", "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef").await;
    assert!(unknown.is_err(), "unknown commit id should error, not silently return None/old data");
}

async fn aruaru_commit(pool: &PgPool, message: &str) -> String {
    use sqlx::Row;
    // aruaru-wire's `DescribePortalResponse` is always empty (dynamic
    // schema, RowDescription only attached at Execute time) — sqlx's
    // extended-query `query_as`/`query` path relies on Describe to know
    // column shape and fails against it. `raw_sql` uses the simple query
    // protocol instead, which this server answers with real column data.
    let mut rows = sqlx::raw_sql(&format!("SELECT aruaru_commit('{}')", message.replace('\'', "''")))
        .fetch_all(pool)
        .await
        .expect("aruaru_commit");
    let row = rows.pop().expect("aruaru_commit returned a row");
    row.try_get::<String, _>(0).expect("aruaru_commit column 0")
}

async fn connect_with_retry(url: &str) -> PgPool {
    let mut last_err = None;
    for _ in 0..50 {
        match PgPool::connect(url).await {
            Ok(pool) => return pool,
            Err(e) => {
                last_err = Some(e);
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
    panic!("could not connect to aruaru-server pgwire endpoint: {last_err:?}");
}
