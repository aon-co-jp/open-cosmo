//! open-web-server の `TenantRegistry`(`TenantConfig{host, backend_addr, ..}`)と
//! 本クレートの `Dispatcher` を橋渡しするアダプタ。
//!
//! open-web-server 側はクロスリポジトリ依存を避けるため、この関数へ
//! `(host, backend_addr)` のペア列を渡すだけでよい(型依存なし)。

use crate::{Dispatcher, StaticDispatcher, UpstreamAddr};
use std::collections::HashMap;

/// `backend_addr` 文字列("127.0.0.1:8080" / "example.internal:9000" /
/// "http://127.0.0.1:8080" のいずれも許容)を `UpstreamAddr` に解析する。
pub fn parse_backend_addr(addr: &str) -> Option<UpstreamAddr> {
    let a = addr
        .trim()
        .strip_prefix("http://")
        .or_else(|| addr.trim().strip_prefix("https://"))
        .unwrap_or(addr.trim());
    let a = a.trim_end_matches('/');
    let (host, port) = a.rsplit_once(':')?;
    let port: u16 = port.parse().ok()?;
    if host.is_empty() {
        return None;
    }
    Some(UpstreamAddr {
        host: host.to_string(),
        port,
    })
}

/// TenantRegistry 由来の (host, backend_addr) ペア列から Dispatcher を構築する。
/// 解析できないエントリは戻り値の2要素目(拒否リスト)に host 名で報告する
/// (金融系用途で「黙って落とす」ことをしないため — §0 監査性)。
pub fn dispatcher_from_tenants<'a>(
    pairs: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> (TenantDispatcher, Vec<String>) {
    let mut routes = HashMap::new();
    let mut rejected = vec![];
    for (host, addr) in pairs {
        match parse_backend_addr(addr) {
            Some(up) => {
                routes.insert(host.to_ascii_lowercase(), up);
            }
            None => rejected.push(host.to_string()),
        }
    }
    (TenantDispatcher { routes }, rejected)
}

/// `dispatcher_from_tenants` 専用の不変 Dispatcher。
/// 動的追加が必要な場面では `StaticDispatcher` を使う。
pub struct TenantDispatcher {
    routes: HashMap<String, UpstreamAddr>,
}

impl Dispatcher for TenantDispatcher {
    fn resolve(&self, host: &str) -> Option<UpstreamAddr> {
        let h = host.split(':').next().unwrap_or(host).to_ascii_lowercase();
        self.routes.get(&h).cloned()
    }
}

/// 既存の `StaticDispatcher` にもペア列から流し込めるようにする補助。
pub fn extend_static_dispatcher<'a>(
    d: &mut StaticDispatcher,
    pairs: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Vec<String> {
    let mut rejected = vec![];
    for (host, addr) in pairs {
        match parse_backend_addr(addr) {
            Some(up) => d.register_addr(host, up),
            None => rejected.push(host.to_string()),
        }
    }
    rejected
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_scheme_and_trailing_slash_forms() {
        for s in ["127.0.0.1:8080", "http://127.0.0.1:8080", "http://127.0.0.1:8080/"] {
            let u = parse_backend_addr(s).unwrap();
            assert_eq!((u.host.as_str(), u.port), ("127.0.0.1", 8080), "{s}");
        }
        assert!(parse_backend_addr("no-port").is_none());
        assert!(parse_backend_addr(":8080").is_none());
    }

    #[test]
    fn builds_dispatcher_and_reports_rejects() {
        let (d, rejected) = dispatcher_from_tenants([
            ("Shop.Example.JP", "http://127.0.0.1:4100"),
            ("bad.example", "not-an-addr"),
        ]);
        assert_eq!(d.resolve("shop.example.jp:443").unwrap().port, 4100);
        assert_eq!(rejected, vec!["bad.example".to_string()]);
    }
}
