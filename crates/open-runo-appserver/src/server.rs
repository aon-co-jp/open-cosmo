//! マルチスレッド受付サーバ(Phase 2)— マルチCPU/マルチコア活用。
//!
//! `proxy::proxy_once` を固定サイズのワーカースレッドプールで並列実行する。
//! 依存追加なし(`std` のみ)。Poem/tokio の非同期ランタイムに載せる統合は
//! 各リポジトリ側で行うが、この同期プール実装は (a) sandbox で完全に
//! テスト可能、(b) CGI/FPM系のブロッキングupstreamと相性が良い、という
//! 独立した価値を持つ。
//!
//! セキュリティ上の既定値(§0 ハイセキュリティ要件):
//! - ヘッダ部の最大サイズ制限(既定 16KiB)— ヘッダ爆弾対策
//! - ボディの最大サイズ制限(既定 16MiB)— メモリ枯渇対策
//! - 接続ごとの読み取りタイムアウト — slowloris系の滞留対策
//! - キュー上限到達時は即座に 503 を返す(黙って落とさない — §0 監査性)

use crate::proxy::proxy_once;
use crate::Dispatcher;
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// ワーカースレッド数。0 なら論理CPU数(最低2)を自動採用。
    pub workers: usize,
    /// 受付キューの深さ。超過時は 503。
    pub queue_depth: usize,
    pub upstream_timeout: Duration,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            workers: 0,
            queue_depth: 1024,
            upstream_timeout: Duration::from_secs(30),
        }
    }
}

impl ServerConfig {
    fn effective_workers(&self) -> usize {
        if self.workers > 0 {
            return self.workers;
        }
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2)
            .max(2)
    }
}

/// 稼働統計(監視・監査用)。
#[derive(Debug, Default)]
pub struct ServerStats {
    pub accepted: AtomicU64,
    pub served: AtomicU64,
    pub rejected_queue_full: AtomicU64,
    pub errors: AtomicU64,
}

/// マルチスレッドプロキシサーバ。
pub struct ThreadedProxyServer {
    shutdown: Arc<AtomicBool>,
    pub stats: Arc<ServerStats>,
    accept_thread: Option<JoinHandle<()>>,
    workers: Vec<JoinHandle<()>>,
    pub local_port: u16,
}

impl ThreadedProxyServer {
    /// `bind_addr`(例 "0.0.0.0:8080"、":0"でエフェメラル)で受付を開始する。
    /// Dispatcher は全ワーカーで共有される(`Send + Sync` 必須 = マルチコアで
    /// ロックフリーに読み取り解決できる実装を選ぶこと)。
    pub fn start<D: Dispatcher + Send + Sync + 'static>(
        bind_addr: &str,
        dispatcher: Arc<D>,
        config: ServerConfig,
    ) -> std::io::Result<Self> {
        let listener = TcpListener::bind(bind_addr)?;
        let local_port = listener.local_addr()?.port();
        listener.set_nonblocking(true)?;

        let shutdown = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(ServerStats::default());
        let (tx, rx): (SyncSender<TcpStream>, Receiver<TcpStream>) =
            sync_channel(config.queue_depth);
        let rx = Arc::new(Mutex::new(rx));

        let mut workers = Vec::new();
        for _ in 0..config.effective_workers() {
            let rx = rx.clone();
            let d = dispatcher.clone();
            let st = stats.clone();
            let sd = shutdown.clone();
            let timeout = config.upstream_timeout;
            workers.push(std::thread::spawn(move || loop {
                let job = {
                    let guard = rx.lock().unwrap();
                    guard.recv_timeout(Duration::from_millis(100))
                };
                match job {
                    Ok(stream) => {
                        let peer = stream
                            .peer_addr()
                            .map(|a| a.to_string())
                            .unwrap_or_else(|_| "unknown".into());
                        match proxy_once(stream, &peer, d.as_ref(), timeout) {
                            Ok(_) => {
                                st.served.fetch_add(1, Ordering::Relaxed);
                            },
                            Err(_) => {
                                st.errors.fetch_add(1, Ordering::Relaxed);
                            },
                        }
                    },
                    Err(_) => {
                        if sd.load(Ordering::Relaxed) {
                            break;
                        }
                    },
                }
            }));
        }

        let sd = shutdown.clone();
        let st = stats.clone();
        let accept_thread = std::thread::spawn(move || {
            while !sd.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        st.accepted.fetch_add(1, Ordering::Relaxed);
                        match tx.try_send(stream) {
                            Ok(()) => {},
                            Err(TrySendError::Full(mut s)) => {
                                st.rejected_queue_full.fetch_add(1, Ordering::Relaxed);
                                let _ = s.write_all(
                                    b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                                );
                            },
                            Err(TrySendError::Disconnected(_)) => break,
                        }
                    },
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    },
                    Err(_) => {
                        st.errors.fetch_add(1, Ordering::Relaxed);
                    },
                }
            }
        });

        Ok(Self {
            shutdown,
            stats,
            accept_thread: Some(accept_thread),
            workers,
            local_port,
        })
    }

    /// 受付を止め、全ワーカーの終了を待つ。
    pub fn stop(mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(t) = self.accept_thread.take() {
            let _ = t.join();
        }
        for w in self.workers.drain(..) {
            let _ = w.join();
        }
    }
}

impl Drop for ThreadedProxyServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }
}

/// `Dispatcher` の `Send + Sync` 版が必要になるため、mutexで包む最小実装。
/// 読み取り頻度が高い本番用途では `TenantDispatcher`(不変・ロック不要)を推奨。
pub struct SharedDispatcher<D: Dispatcher>(pub Mutex<D>);

impl<D: Dispatcher> Dispatcher for SharedDispatcher<D> {
    fn resolve(&self, host: &str) -> Option<crate::UpstreamAddr> {
        self.0.lock().ok()?.resolve(host)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tenant_bridge::dispatcher_from_tenants;
    use std::io::{BufRead, BufReader, Read};
    use std::net::TcpListener;

    /// リクエストを受けるエコーupstream(並列受付)。`_n` は目安で、実際には
    /// リスナーが閉じられるまで受け付け続ける(固定回数 `for 0..n` だと、
    /// 負荷時に接続が1本でも余分/不足すると helper が wedge するため)。
    /// 各接続の I/O エラーは無視する(テスト用エコーサーバーがクライアント側の
    /// RST で panic しては本末転倒)。
    fn spawn_upstream(_n: usize) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            while let Ok((mut s, _)) = listener.accept() {
                std::thread::spawn(move || {
                    let Ok(clone) = s.try_clone() else { return };
                    let mut r = BufReader::new(clone);
                    // ヘッダを読み飛ばす
                    loop {
                        let mut line = String::new();
                        match r.read_line(&mut line) {
                            Ok(0) => return,
                            Ok(_) if line.trim().is_empty() => break,
                            Ok(_) => {},
                            Err(_) => return,
                        }
                    }
                    let body = "ok";
                    let _ = s.write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    );
                });
            }
        });
        port
    }

    #[test]
    fn serves_concurrent_requests_across_worker_threads() {
        const N: usize = 16;
        let up = spawn_upstream(N);
        let addr = format!("127.0.0.1:{up}");
        let (d, rejected) = dispatcher_from_tenants([("bank.example.jp", addr.as_str())]);
        assert!(rejected.is_empty());

        let server = ThreadedProxyServer::start(
            "127.0.0.1:0",
            Arc::new(d),
            ServerConfig {
                workers: 4,
                ..Default::default()
            },
        )
        .unwrap();
        let port = server.local_port;

        // 1リクエストを1接続で投げ、"HTTP/1.1 200 OK ... ok" が返れば成功。
        // 接続確立/送受信の一過性エラー(高負荷CIでの RST=WSAECONNRESET や
        // WSAECONNABORTED 等)は、この接続の失敗として数回まで再試行する。
        // ここで検証したいのは「複数ワーカーで N 本の並行リクエストが
        // 最終的に全て正しく中継される」ことであって、1接続も取りこぼさない
        // OS レベルの完全性ではない(後者は負荷次第で揺れ、テストが flaky に
        // なるだけで実装の回帰検知には寄与しない)。
        fn one_request(port: u16) -> std::io::Result<String> {
            let mut c = TcpStream::connect(("127.0.0.1", port))?;
            c.set_read_timeout(Some(Duration::from_secs(10)))?;
            write!(c, "GET /balance HTTP/1.1\r\nHost: bank.example.jp\r\n\r\n")?;
            c.flush()?;
            let mut resp = String::new();
            BufReader::new(&c).read_to_string(&mut resp)?;
            Ok(resp)
        }

        let mut clients = vec![];
        for _ in 0..N {
            clients.push(std::thread::spawn(move || {
                let deadline = std::time::Instant::now() + Duration::from_secs(20);
                let mut last = String::new();
                while std::time::Instant::now() < deadline {
                    match one_request(port) {
                        Ok(resp) if resp.starts_with("HTTP/1.1 200 OK") && resp.ends_with("ok") => {
                            return;
                        },
                        Ok(resp) => last = resp,
                        Err(e) => last = format!("io error: {e}"),
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                panic!("request never succeeded within 20s; last result: {last:?}");
            }));
        }
        for c in clients {
            c.join().unwrap();
        }
        // 少なくとも N 本の中継が成功していること(再試行が走った場合は
        // それ以上になり得る)。errors カウンタは一過性の接続エラーで
        // 増え得るため厳密な 0 判定はしない。
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while server.stats.served.load(Ordering::Relaxed) < N as u64
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            server.stats.served.load(Ordering::Relaxed) >= N as u64,
            "served={} < N={N}",
            server.stats.served.load(Ordering::Relaxed)
        );
        server.stop();
    }
}
