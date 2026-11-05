//! UDP 即時通知の受信側(open-cosmo では無効化)。
//!
//! RPoem では `open-web-server-wire` の `UdpReceiver` で受信するが、open-cosmo は
//! 兄弟リポジトリへの path 依存を持たない方針(単独でクローン・ビルドできること)のため、
//! リスナーは実装しない。カウンタ型と `spawn_from_env` だけを同じ形で残し、
//! 常に「起動しない」(`None`)を返す。UDP 通知が要るなら RPoem 側のルーターを使う。

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// 受信した UDP 通知の観測用カウンタ(open-cosmo では常に 0)。
#[derive(Debug, Default)]
pub struct NoticeStats {
    received: AtomicU64,
    duplicate_or_invalid: AtomicU64,
}

impl NoticeStats {
    pub fn received(&self) -> u64 {
        self.received.load(Ordering::Relaxed)
    }

    pub fn duplicate_or_invalid(&self) -> u64 {
        self.duplicate_or_invalid.load(Ordering::Relaxed)
    }
}

/// 常に `None`(open-cosmo ではリスナーを起動しない)。
pub async fn spawn_from_env(
    _stats: Arc<NoticeStats>,
) -> Option<(SocketAddr, tokio::task::JoinHandle<()>)> {
    if std::env::var("OPEN_RUNO_UDP_NOTICE_BIND").is_ok() {
        tracing::warn!("OPEN_RUNO_UDP_NOTICE_BIND is set, but open-cosmo does not include the UDP notice listener (use RPoem)");
    }
    None
}
