//! アプリケーションサーバー層(「第二のTomcat」)の管理操作。
//!
//! 旧`/admin/appserver-processes*`・`/admin/appserver-tenants*`のREST 10本を、
//! `open_runo.admin.v1.Admin`のRPC(`AppServer*`、gRPC / gRPC-web / Connect で同じ定義)へ
//! 移したもの(REST全廃、2026-11-07)。認証・上限・監査は`AdminGrpc`側で済ませてから呼ばれる。
//! 要求・応答は`google.protobuf.Struct`(旧RESTと同じJSON形)。旧RESTの経路は削除済み。
//!
//! `SupervisedTenantRegistry`は「テナント登録(host→backend_addr)」と「そのバックエンドの
//! 起動・監視」を束ねる単一のレジストリで、テナント系もプロセス系もこれを共有する。

use std::sync::Arc;
use std::time::Duration;

use open_runo_appserver::tenant_bridge::SupervisedTenantRegistry;
use open_runo_appserver::{RestartPolicy, RuntimeProfile};
use open_runo_proto::code;
use open_runo_router::grpc_admin::{AdminExt, GrpcErr};
use serde_json::{json, Value};

/// `AdminGrpc::with_ext`に載せる実体。
pub struct AppServerAdmin {
    registry: Arc<SupervisedTenantRegistry>,
}

impl AppServerAdmin {
    pub fn new(registry: Arc<SupervisedTenantRegistry>) -> Self {
        Self { registry }
    }
}

fn bad(msg: impl Into<String>) -> GrpcErr {
    GrpcErr::new(code::INVALID_ARGUMENT, msg)
}

fn no_process(host: &str) -> GrpcErr {
    GrpcErr::new(
        code::NOT_FOUND,
        format!("no managed process for host: {host}"),
    )
}

fn host_of(req: &Value) -> Result<String, GrpcErr> {
    match req.get("host").and_then(Value::as_str) {
        Some(h) if !h.is_empty() => Ok(h.to_string()),
        _ => Err(bad("host is required")),
    }
}

fn ms_of(req: &Value, key: &str) -> Result<Option<u64>, GrpcErr> {
    match req.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .map(Some)
            .ok_or_else(|| bad(format!("{key} must be a non-negative integer"))),
    }
}

fn timeout_of(req: &Value) -> Result<Duration, GrpcErr> {
    Ok(Duration::from_millis(
        ms_of(req, "timeout_ms")?.unwrap_or(5000),
    ))
}

#[async_trait::async_trait]
impl AdminExt for AppServerAdmin {
    async fn call(
        &self,
        method: &str,
        _actor: &str,
        req: Value,
    ) -> Option<Result<Value, GrpcErr>> {
        let reg = Arc::clone(&self.registry);
        Some(match method {
            "AppServerRegisterProcess" => register_process(reg, req).await,
            "AppServerListProcesses" => {
                let entries: Vec<Value> = reg
                    .list_managed()
                    .into_iter()
                    .map(|(host, st)| json!({ "host": host, "status": st }))
                    .collect();
                Ok(json!({ "processes": entries }))
            },
            "AppServerProcessStatus" => host_of(&req).map(|host| {
                // 管理対象でないホストは`status: null`(「管理していない」と「異常」を混同しない)。
                let status = reg.process_status(&host);
                json!({ "host": host, "status": status })
            }),
            "AppServerStopProcess" => host_of(&req).and_then(|host| {
                if reg.stop_process(&host) {
                    Ok(json!({ "status": "stopped", "host": host }))
                } else {
                    Err(no_process(&host))
                }
            }),
            "AppServerRestartProcess" => host_of(&req).and_then(|host| {
                if reg.restart_process(&host) {
                    Ok(json!({ "status": "restarted", "host": host }))
                } else {
                    Err(no_process(&host))
                }
            }),
            "AppServerStopProcessGraceful" => {
                let (host, timeout) = match (host_of(&req), timeout_of(&req)) {
                    (Ok(h), Ok(t)) => (h, t),
                    (Err(e), _) | (_, Err(e)) => return Some(Err(e)),
                };
                let h = host.clone();
                let outcome =
                    tokio::task::spawn_blocking(move || reg.stop_process_graceful(&h, timeout))
                        .await
                        .unwrap_or(None);
                match outcome {
                    Some(graceful) => Ok(json!({ "host": host, "graceful": graceful })),
                    None => Err(no_process(&host)),
                }
            },
            "AppServerStopAll" => {
                let graceful = req
                    .get("graceful")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let timeout = match timeout_of(&req) {
                    Ok(t) => t,
                    Err(e) => return Some(Err(e)),
                };
                if graceful {
                    let results =
                        tokio::task::spawn_blocking(move || reg.stop_all_graceful(timeout))
                            .await
                            .unwrap_or_default();
                    let entries: Vec<Value> = results
                        .into_iter()
                        .map(|(host, graceful)| json!({ "host": host, "graceful": graceful }))
                        .collect();
                    Ok(json!({ "stopped": entries }))
                } else {
                    Ok(json!({ "stopped": reg.stop_all() }))
                }
            },
            "AppServerUpsertTenant" => upsert_tenant(&reg, &req),
            "AppServerListTenants" => {
                let entries: Vec<Value> = reg
                    .dispatcher()
                    .list()
                    .into_iter()
                    .map(|(host, a)| {
                        json!({ "host": host, "backend_addr": format!("{}:{}", a.host, a.port) })
                    })
                    .collect();
                Ok(json!({ "tenants": entries }))
            },
            "AppServerRemoveTenant" => host_of(&req).map(|host| {
                // 未登録でも冪等に成功する。
                reg.dispatcher().remove(&host);
                json!({ "status": "removed", "host": host })
            }),
            _ => return None,
        })
    }
}

fn upsert_tenant(reg: &SupervisedTenantRegistry, req: &Value) -> Result<Value, GrpcErr> {
    let host = host_of(req)?;
    let addr_s = req
        .get("backend_addr")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("backend_addr is required"))?;
    let addr = open_runo_appserver::tenant_bridge::parse_backend_addr(addr_s)
        .ok_or_else(|| bad(format!("invalid backend_addr: {addr_s}")))?;
    reg.dispatcher().upsert(&host, addr);
    Ok(json!({ "status": "registered", "host": host }))
}

/// 子プロセスを起動する(ブロッキング呼び出しなので`spawn_blocking`へ退避)。
async fn register_process(
    reg: Arc<SupervisedTenantRegistry>,
    req: Value,
) -> Result<Value, GrpcErr> {
    let host = host_of(&req)?;
    let profile: RuntimeProfile = serde_json::from_value(
        req.get("profile")
            .cloned()
            .ok_or_else(|| bad("profile is required"))?,
    )
    .map_err(|e| bad(format!("invalid profile: {e}")))?;
    let policy: RestartPolicy = match req.get("restart_policy") {
        None | Some(Value::Null) => RestartPolicy::default(),
        Some(v) => serde_json::from_value(v.clone())
            .map_err(|e| bad(format!("invalid restart_policy: {e}")))?,
    };
    let poll = Duration::from_millis(ms_of(&req, "poll_interval_ms")?.unwrap_or(1000));
    let h = host.clone();
    tokio::task::spawn_blocking(move || reg.register_with_managed_process(&h, profile, policy, poll))
        .await
        .map_err(|_| GrpcErr::new(code::INTERNAL, "failed to start managed process"))?;
    Ok(json!({ "status": "registered", "host": host }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn svc() -> AppServerAdmin {
        AppServerAdmin::new(Arc::new(SupervisedTenantRegistry::new()))
    }

    async fn call(s: &AppServerAdmin, m: &str, v: Value) -> Result<Value, GrpcErr> {
        s.call(m, "test", v).await.expect("handled")
    }

    #[tokio::test]
    async fn unknown_method_is_not_handled() {
        assert!(svc().call("Nope", "t", json!({})).await.is_none());
    }

    #[tokio::test]
    async fn tenants_upsert_list_remove() {
        let s = svc();
        let r = call(
            &s,
            "AppServerUpsertTenant",
            json!({"host":"a.example.jp","backend_addr":"127.0.0.1:9001"}),
        )
        .await
        .unwrap();
        assert_eq!(r["status"], "registered");
        let l = call(&s, "AppServerListTenants", json!({})).await.unwrap();
        assert_eq!(l["tenants"][0]["backend_addr"], "127.0.0.1:9001");
        let e = call(
            &s,
            "AppServerUpsertTenant",
            json!({"host":"a","backend_addr":"not an addr"}),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(e.code, code::INVALID_ARGUMENT);
        let e = call(
            &s,
            "AppServerUpsertTenant",
            json!({"backend_addr":"127.0.0.1:1"}),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(e.code, code::INVALID_ARGUMENT);
        call(&s, "AppServerRemoveTenant", json!({"host":"a.example.jp"}))
            .await
            .unwrap();
        call(&s, "AppServerRemoveTenant", json!({"host":"a.example.jp"}))
            .await
            .unwrap();
        let l = call(&s, "AppServerListTenants", json!({})).await.unwrap();
        assert_eq!(l["tenants"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn process_ops_on_unmanaged_host() {
        let s = svc();
        for m in [
            "AppServerStopProcess",
            "AppServerRestartProcess",
            "AppServerStopProcessGraceful",
        ] {
            let e = call(&s, m, json!({"host":"none.example.jp"}))
                .await
                .err()
                .unwrap();
            assert_eq!(e.code, code::NOT_FOUND, "{m}");
        }
        let st = call(&s, "AppServerProcessStatus", json!({"host":"none.example.jp"}))
            .await
            .unwrap();
        assert!(st["status"].is_null());
        let l = call(&s, "AppServerListProcesses", json!({})).await.unwrap();
        assert_eq!(l["processes"].as_array().unwrap().len(), 0);
        let a = call(&s, "AppServerStopAll", json!({})).await.unwrap();
        assert_eq!(a["stopped"].as_array().unwrap().len(), 0);
        let e = call(&s, "AppServerStopAll", json!({"timeout_ms":"x"}))
            .await
            .err()
            .unwrap();
        assert_eq!(e.code, code::INVALID_ARGUMENT);
        let e = call(&s, "AppServerRegisterProcess", json!({"host":"x"}))
            .await
            .err()
            .unwrap();
        assert_eq!(e.code, code::INVALID_ARGUMENT);
    }
}
