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
                            }
                            Err(_) => {
                                st.errors.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                    Err(_) => {
                        if sd.load(Ordering::Relaxed) {
                            break;
                        }
                    }
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
                            Ok(()) => {}
                            Err(TrySendError::Full(mut s)) => {
                                st.rejected_queue_full.fetch_add(1, Ordering::Relaxed);
                                let _ = s.write_all(
                                    b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                                );
                            }
                            Err(TrySendError::Disconnected(_)) => break,
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => {
                        st.errors.fetch_add(1, Ordering::Relaxed);
                    }
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

    /// N リクエストを受けるエコーupstream(並列受付)。
    fn spawn_upstream(n: usize) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for _ in 0..n {
                let (mut s, _) = listener.accept().unwrap();
                std::thread::spawn(move || {
                    let mut r = BufReader::new(s.try_clone().unwrap());
                    // ヘッダを読み飛ばす
                    loop {
                        let mut line = String::new();
                        if r.read_line(&mut line).unwrap() == 0 || line.trim().is_empty() {
                            break;
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

        let mut clients = vec![];
        for _ in 0..N {
            clients.push(std::thread::spawn(move || {
                let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
                write!(c, "GET /balance HTTP/1.1\r\nHost: bank.example.jp\r\n\r\n").unwrap();
                let mut resp = String::new();
                BufReader::new(&c).read_to_string(&mut resp).unwrap();
                assert!(resp.starts_with("HTTP/1.1 200 OK"), "{resp}");
                assert!(resp.ends_with("ok"), "{resp}");
            }));
        }
        for c in clients {
            c.join().unwrap();
        }
        // served の加算はクライアントへの書き込み完了後に行われるため、
        // クライアント側の read 完了と厳密には同期しない。収束を待つ。
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while server.stats.served.load(Ordering::Relaxed) < N as u64
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(server.stats.served.load(Ordering::Relaxed), N as u64);
        assert_eq!(server.stats.errors.load(Ordering::Relaxed), 0);
        server.stop();
    }
}
