//! 管理用 gRPC サービス `open_runo.admin.v1.Admin`(REST全廃・gRPC移行、CLAUDE.md「2026-11-01追記」)。
//!
//! RESTハンドラやGraphQLの`admin`と同じく、処理は通信方式に依存しない共通関数
//! ([`crate::admin_ops`]・[`crate::admin_sys`])に任せ、ここは「プロトコル⇔共通関数」の変換だけを持つ
//! (二重実装しない)。定義の正は`open-runo-proto`の`admin.proto`。
//!
//! 提供する通信路:
//! - 素のgRPC(HTTP/2、`grpc::serve_grpc_with` / TLS1.3は`grpc::serve_grpc_tls`)。CLI・呼び出し側向け。
//! - gRPC-web(HTTP/1.1でも動く、`application/grpc-web+proto`)。ブラウザのWASM向け。
//!   通常のHTTPルーター(`hyper_compat::Router`)に[`grpc_web_handler`]を載せて使う。
//!
//! 認証: gRPCメタデータ(=HTTPヘッダ)の`x-api-key`、またはセッション(`cookie`+`x-csrf-token`)を
//! [`authenticate_with_session`]で検査する(REST時代と同じKeyGuardian/セッション)。権限もREST時代と同じ
//! (認証済みなら実行可)。`SelfIssueKey`/`SessionLogin`/`SessionLogout`だけ入口として鍵なしで呼べる。
//! 更新系は共通関数が監査ログへ記録する。

use crate::admin_ops::{self, AdminError, ComposeService};
use crate::admin_sys::{self, RouteCandidate};
use crate::auth_hyper::authenticate_with_session;
use crate::keyring::KeyGuardian;
use crate::state::AppState;
use futures::Stream;
use hyper::{HeaderMap, Method};
use open_runo_api_types::{FeatureFlagRequest, RegisterSchemaRequest};
use open_runo_proto::prost::Message;
use open_runo_proto::{code, json_to_struct, v1, value_to_json};
use serde::Serialize;
use std::collections::BTreeMap;
use std::pin::Pin;
use std::sync::Arc;

/// 要求1件あたりの最大サイズ(これを超えるとRESOURCE_EXHAUSTED)。
pub const MAX_REQUEST_BYTES: usize = 4 * 1024 * 1024;

/// gRPCのエラー(コード+メッセージ)。
#[derive(Debug, Clone)]
pub struct GrpcErr {
    pub code: i32,
    pub message: String,
}

impl GrpcErr {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl From<AdminError> for GrpcErr {
    fn from(e: AdminError) -> Self {
        Self::new(code::from_http(e.status.as_u16()), e.message)
    }
}

type Stream_ = Pin<Box<dyn Stream<Item = Vec<u8>> + Send + Sync>>;

/// 応答本体。
pub enum Reply {
    Unary(Vec<u8>),
    Stream(Stream_),
}

/// 呼び出し結果(本体 + 応答メタデータ。`SessionLogin`の`set-cookie`など)。
pub struct Output {
    pub reply: Reply,
    pub headers: Vec<(&'static str, String)>,
}

impl Output {
    fn unary(bytes: Vec<u8>) -> Self {
        Self {
            reply: Reply::Unary(bytes),
            headers: Vec::new(),
        }
    }
}

/// 管理サービスの拡張(ゲートウェイ側にしか無い機能、例: アプリケーションサーバー層)。
/// 認証は`AdminGrpc`が済ませてから呼ぶ。要求・応答は`google.protobuf.Struct`と同じJSONオブジェクト。
/// 担当しないメソッド名には`None`を返す(→UNIMPLEMENTED)。
#[async_trait::async_trait]
pub trait AdminExt: Send + Sync {
    async fn call(
        &self,
        method: &str,
        actor: &str,
        msg: serde_json::Value,
    ) -> Option<Result<serde_json::Value, GrpcErr>>;
}

/// `open_runo.admin.v1.Admin`の実体。
#[derive(Clone)]
pub struct AdminGrpc {
    state: Arc<AppState>,
    guardian: Arc<KeyGuardian>,
    ext: Option<Arc<dyn AdminExt>>,
}

impl std::fmt::Debug for AdminGrpc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AdminGrpc")
    }
}

fn dec<M: Message + Default>(bytes: &[u8]) -> Result<M, GrpcErr> {
    M::decode(bytes)
        .map_err(|e| GrpcErr::new(code::INVALID_ARGUMENT, format!("bad request message: {e}")))
}

fn enc<M: Message>(m: &M) -> Output {
    Output::unary(m.encode_to_vec())
}

/// RESTと同じJSON形の結果を`google.protobuf.Struct`で返す。
fn st<T: Serialize>(v: &T) -> Output {
    let json = serde_json::to_value(v).unwrap_or(serde_json::Value::Null);
    enc(&json_to_struct(&json))
}

fn schema_version(v: open_runo_api_types::SchemaVersion) -> v1::SchemaVersion {
    v1::SchemaVersion {
        id: v.id,
        namespace: v.namespace,
        service_name: v.service_name,
        sdl: v.sdl,
        stage: v.stage,
        created_at: v.created_at,
    }
}

fn flag(f: open_runo_api_types::FeatureFlagResponse) -> v1::FeatureFlag {
    v1::FeatureFlag {
        name: f.name,
        enabled: f.enabled,
        rollout_percent: u32::from(f.rollout_percent),
        description: f.description,
    }
}

fn persisted(r: admin_ops::PersistedQueryRecord) -> v1::PersistedQuery {
    v1::PersistedQuery {
        hash: r.hash,
        query: r.query,
        registered_at: r.registered_at,
    }
}

impl AdminGrpc {
    pub fn new(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Self {
        Self {
            state,
            guardian,
            ext: None,
        }
    }

    /// 拡張(`AdminExt`)を載せる。
    pub fn with_ext(mut self, ext: Arc<dyn AdminExt>) -> Self {
        self.ext = Some(ext);
        self
    }

    /// 呼び出し元(REST版の`actor`文字列と同じ形式)。認証失敗はUNAUTHENTICATED/PERMISSION_DENIED。
    async fn actor(&self, headers: &HeaderMap) -> Result<String, GrpcErr> {
        match authenticate_with_session(
            headers,
            &Method::POST,
            &self.guardian,
            &self.state.sessions,
        )
        .await
        {
            Ok(a) => Ok(format!(
                "{}{}",
                if a.via_session { "session:" } else { "" },
                a.owner
            )),
            Err(status) => Err(GrpcErr::new(
                code::from_http(status.as_u16()),
                "authentication required (x-api-key or session metadata)",
            )),
        }
    }

    /// `method`(例: `RegisterSchema`)を実行する。
    pub async fn call(
        &self,
        method: &str,
        headers: &HeaderMap,
        msg: &[u8],
    ) -> Result<Output, GrpcErr> {
        let state = &self.state;
        // 鍵なしで呼べる入口。
        match method {
            "SelfIssueKey" => {
                let r = admin_sys::self_issue_key(state, &self.guardian).await?;
                return Ok(enc(&v1::SelfIssued {
                    api_key: r.api_key,
                    expires_at: r.expires_at,
                }));
            },
            "SessionLogin" => {
                let key = headers
                    .get("x-api-key")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");
                let (login, cookie) =
                    admin_sys::session_login(&self.guardian, &state.sessions, key).await?;
                let mut out = enc(&v1::SessionLoginResponse {
                    owner: login.owner,
                    csrf_token: login.csrf_token,
                    expires_in_secs: login.expires_in_secs,
                });
                out.headers.push(("set-cookie", cookie));
                return Ok(out);
            },
            "SessionLogout" => {
                let sid = crate::session::session_id_from_cookie_header(headers);
                let cookie = admin_sys::session_logout(&state.sessions, sid.as_deref()).await;
                let mut out = enc(&v1::Empty {});
                out.headers.push(("set-cookie", cookie));
                return Ok(out);
            },
            _ => {},
        }

        let actor = self.actor(headers).await?;
        let a = actor.as_str();
        Ok(match method {
            // ── スキーマ / Federation ──
            "RegisterSchema" => {
                let r: v1::RegisterSchemaRequest = dec(msg)?;
                let v = admin_ops::register_schema(
                    state,
                    a,
                    RegisterSchemaRequest {
                        service_name: r.service_name,
                        sdl: r.sdl,
                        stage: r.stage.unwrap_or_else(|| "local".to_string()),
                        namespace: r.namespace,
                    },
                )
                .await;
                enc(&schema_version(v))
            },
            "GetSchema" => {
                let r: v1::GetSchemaRequest = dec(msg)?;
                let v = admin_ops::get_schema(
                    state,
                    r.namespace.as_deref(),
                    &r.service_name,
                    r.stage.as_deref(),
                )?;
                enc(&schema_version(v))
            },
            "SchemaHistory" => {
                let r: v1::SchemaHistoryRequest = dec(msg)?;
                let versions =
                    admin_ops::schema_history(state, r.namespace.as_deref(), &r.service_name)
                        .into_iter()
                        .map(schema_version)
                        .collect();
                enc(&v1::SchemaHistoryResponse { versions })
            },
            "FederationStatus" => {
                let s = admin_ops::federation_status(state);
                enc(&v1::FederationStatusResponse {
                    contributing_services: s.contributing_services,
                    type_count: s.type_count as u64,
                    field_count: s.field_count as u64,
                })
            },
            "ComposeFederation" => {
                let r: v1::ComposeRequest = dec(msg)?;
                let services = r
                    .services
                    .into_iter()
                    .map(|s| ComposeService {
                        service_name: s.service_name,
                        types: s
                            .types
                            .into_iter()
                            .map(|(k, v)| (k, v.fields))
                            .collect::<BTreeMap<_, _>>(),
                        sdl: s.sdl,
                    })
                    .collect();
                let out = admin_ops::compose(state, services)?;
                enc(&v1::ComposeResponse {
                    contributing_services: out.contributing_services,
                    types: out
                        .types
                        .into_iter()
                        .map(|(k, fields)| (k, v1::FieldList { fields }))
                        .collect(),
                    breaking_changes: out.breaking_changes,
                })
            },
            // ── フィーチャーフラグ ──
            "UpsertFeatureFlag" => {
                let r: v1::UpsertFeatureFlagRequest = dec(msg)?;
                let rollout = u8::try_from(r.rollout_percent.unwrap_or(100)).map_err(|_| {
                    GrpcErr::new(
                        code::INVALID_ARGUMENT,
                        "rollout_percent must be between 0 and 100",
                    )
                })?;
                let f = admin_ops::flag_upsert(
                    state,
                    a,
                    FeatureFlagRequest {
                        name: r.name,
                        enabled: r.enabled.unwrap_or(true),
                        rollout_percent: rollout,
                        description: r.description.unwrap_or_default(),
                    },
                )
                .await?;
                enc(&flag(f))
            },
            "ListFeatureFlags" => enc(&v1::ListFeatureFlagsResponse {
                flags: admin_ops::flag_list(state).into_iter().map(flag).collect(),
            }),
            "GetFeatureFlag" => {
                let r: v1::FlagName = dec(msg)?;
                enc(&flag(admin_ops::flag_get(state, &r.name)?))
            },
            "DeleteFeatureFlag" => {
                let r: v1::FlagName = dec(msg)?;
                admin_ops::flag_delete(state, a, &r.name).await?;
                enc(&v1::Empty {})
            },
            "EvaluateFeatureFlag" => {
                let r: v1::EvaluateFlagRequest = dec(msg)?;
                let e = admin_ops::flag_evaluate(state, &r.name, &r.bucket_key)?;
                enc(&v1::FlagEvaluation {
                    name: e.name,
                    bucket_key: e.bucket_key,
                    enabled: e.enabled,
                })
            },
            // ── 永続クエリ / キャッシュ ──
            "RegisterPersistedQuery" => {
                let r: v1::RegisterPersistedQueryRequest = dec(msg)?;
                enc(&persisted(
                    admin_ops::persisted_query_register(state, a, &r.query).await?,
                ))
            },
            "GetPersistedQuery" => {
                let r: v1::PersistedQueryHash = dec(msg)?;
                enc(&persisted(
                    admin_ops::persisted_query_get(state, &r.hash).await?,
                ))
            },
            "PurgeCache" => {
                let r: v1::PurgeRequest = dec(msg)?;
                enc(&v1::PurgeResponse {
                    purged: admin_ops::cache_purge(state, &state.page_cache, a, &r.path).await,
                })
            },
            "PurgeAllCache" => enc(&v1::PurgeResponse {
                purged: admin_ops::cache_purge_all(state, &state.page_cache, a).await,
            }),
            "CacheAiStats" => st(&admin_sys::ai_stats(&state.page_cache)),
            // ── DB ──
            "DbStatus" => st(&admin_sys::db_status(state)),
            "DbRouting" => st(&admin_sys::db_routing()),
            "DbList" => {
                let r: v1::DbTable = dec(msg)?;
                st(&admin_sys::db_list(state, &r.table).await?)
            },
            "DbGet" => {
                let r: v1::DbKey = dec(msg)?;
                st(&admin_sys::db_get(state, &r.table, &r.key).await?)
            },
            "DbGetAtCommit" => {
                let r: v1::DbAtCommit = dec(msg)?;
                st(&admin_sys::db_get_at_commit(state, &r.table, &r.key, &r.commit_id).await?)
            },
            "DbPut" => {
                let r: v1::DbPutRequest = dec(msg)?;
                let value = r
                    .value
                    .as_ref()
                    .map(value_to_json)
                    .unwrap_or(serde_json::Value::Null);
                st(&admin_sys::db_put(state, a, &r.table, &r.key, value).await?)
            },
            "DbDelete" => {
                let r: v1::DbKey = dec(msg)?;
                st(&admin_sys::db_delete(state, a, &r.table, &r.key).await?)
            },
            // ── バックアップ / 移行 / 整合性 ──
            "BackupExport" => st(&admin_sys::backup_export(state, &state.page_cache, a).await?),
            "BackupImport" => {
                let r: v1::BackupImportRequest = dec(msg)?;
                st(&admin_sys::backup_import(state, a, &r.path).await?)
            },
            "BackupRestoreLatest" => st(&admin_sys::backup_restore_latest(state, a).await?),
            "MigrateExportSql" => {
                let r: v1::MigrateExportSqlRequest = dec(msg)?;
                let dialect = admin_sys::parse_dialect(&r.dialect)?;
                st(&admin_sys::migrate_export_sql(state, a, dialect).await?)
            },
            "MigrateExportCsv" => st(&admin_sys::migrate_export_csv(state, a).await?),
            "IntegrityCheck" => st(&admin_sys::integrity_check(state, a).await?),
            // ── 分析 / AI経路 ──
            "AnalyticsRequestsPerMonth" => st(&admin_sys::requests_per_month(state)),
            "AnalyticsOperations" => st(&admin_sys::operations_summary(state)),
            "AiRoute" => {
                let r: v1::AiRouteRequest = dec(msg)?;
                let candidates: Vec<RouteCandidate> = r
                    .candidates
                    .into_iter()
                    .map(|c| RouteCandidate {
                        provider: c.provider,
                        estimated_cost_usd_per_1k_tokens: c.estimated_cost_usd_per_1k_tokens,
                        estimated_latency_ms: c.estimated_latency_ms,
                        is_local: c.is_local,
                        context_length: c.context_length,
                    })
                    .collect();
                st(&admin_sys::ai_route(
                    &r.policy,
                    r.min_context_length,
                    &candidates,
                )?)
            },
            // ── SCIM ──
            "ScimListUsers" => {
                let r: v1::ScimListRequest = dec(msg)?;
                st(&admin_sys::scim_list_users(state, r.filter.as_deref()).await?)
            },
            "ScimCreateUser" => {
                let r: v1::ScimCreateUserRequest = dec(msg)?;
                let body = admin_sys::scim_create_user_named(
                    state,
                    &self.guardian,
                    a,
                    &r.user_name,
                    r.roles,
                )
                .await?;
                enc(&json_to_struct(&body))
            },
            "ScimDeleteUser" => {
                let r: v1::ScimUserId = dec(msg)?;
                admin_sys::scim_delete_user(state, &self.guardian, a, &r.id).await?;
                enc(&v1::Empty {})
            },
            // ── Ledger(open-web-server)からの書き込み(旧`POST /internal/db/mutate`) ──
            "DbMutate" => {
                let r: open_runo_proto::prost_types::Struct = dec(msg)?;
                let body = open_runo_proto::struct_to_json(&r);
                match crate::handlers_hyper::db_mutate_core(state, body).await {
                    Ok(v) => enc(&json_to_struct(&v)),
                    Err(("invalid_argument", m)) => {
                        return Err(GrpcErr::new(code::INVALID_ARGUMENT, m))
                    },
                    Err((_, m)) => return Err(GrpcErr::new(code::INTERNAL, m)),
                }
            },
            // ── イベント(サーバーストリーミング) ──
            "WatchEvents" => Output {
                reply: Reply::Stream(self.watch_events()),
                headers: Vec::new(),
            },
            other => {
                if let Some(ext) = &self.ext {
                    let req: open_runo_proto::prost_types::Struct = dec(msg)?;
                    if let Some(r) = ext
                        .call(other, a, open_runo_proto::struct_to_json(&req))
                        .await
                    {
                        return Ok(enc(&json_to_struct(&r?)));
                    }
                }
                return Err(GrpcErr::new(
                    code::UNIMPLEMENTED,
                    format!("unknown method: {other}"),
                ))
            },
        })
    }

    /// 旧`/api/events`(履歴件数の増加)・`/api/ws-events`(スキーマ登録イベント)の置き換え。
    fn watch_events(&self) -> Stream_ {
        let rx = self.state.events.subscribe();
        let schema = futures::stream::unfold(rx, |mut rx| async move {
            loop {
                match rx.recv().await {
                    Ok(ev) => {
                        let m = v1::AdminEvent {
                            event: Some(v1::admin_event::Event::Schema(v1::SchemaEvent {
                                service_name: ev.service_name,
                                stage: ev.stage,
                                at: ev.at,
                            })),
                        };
                        return Some((m.encode_to_vec(), rx));
                    },
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                }
            }
        });
        let state = Arc::clone(&self.state);
        let initial = state.history.lock().map(|h| h.log().len()).unwrap_or(0);
        let history = futures::stream::unfold((state, initial), |(state, mut last)| async move {
            loop {
                tokio::time::sleep(HISTORY_POLL).await;
                let current = state.history.lock().map(|h| h.log().len()).unwrap_or(last);
                if current > last {
                    last = current;
                    let m = v1::AdminEvent {
                        event: Some(v1::admin_event::Event::History(v1::HistoryEvent {
                            history_len: current as u64,
                        })),
                    };
                    return Some((m.encode_to_vec(), (state, last)));
                }
            }
        });
        Box::pin(futures::stream::select(schema, history))
    }
}

const HISTORY_POLL: std::time::Duration = std::time::Duration::from_secs(5);

// ── 通信路(HTTP) ────────────────────────────────────────────────────────

use crate::hyper_compat::{Request, Response};
use open_runo_connect_server as cs;
use std::sync::OnceLock;

pub use open_runo_connect_core::decode_text_body;

/// 管理サービスのパスから`method`を取り出す(`/open_runo.admin.v1.Admin/<method>`)。
pub fn method_of(path: &str) -> Option<&str> {
    cs::method_of(open_runo_proto::ADMIN_SERVICE, path)
}

fn pool() -> &'static prost_reflect::DescriptorPool {
    static POOL: OnceLock<prost_reflect::DescriptorPool> = OnceLock::new();
    POOL.get_or_init(|| {
        prost_reflect::DescriptorPool::decode(open_runo_proto::FILE_DESCRIPTOR_SET)
            .expect("admin descriptor set must decode")
    })
}

/// gRPC / gRPC-web / gRPC-web-text / Connect の共通部品(`open-runo-connect-server`)に載せる。
/// 通信方式ごとの処理は部品側、認証・権限・監査と各RPCの中身はこちら(`AdminGrpc::call`)。
#[async_trait::async_trait]
impl cs::Service for AdminGrpc {
    fn service_name(&self) -> &str {
        open_runo_proto::ADMIN_SERVICE
    }

    fn method(&self, name: &str) -> Option<cs::MethodInfo> {
        let svc = pool().get_service_by_name(open_runo_proto::ADMIN_SERVICE)?;
        let m = svc.methods().find(|m| m.name() == name)?;
        Some(cs::MethodInfo::with_descriptors(
            m.is_server_streaming(),
            m.input(),
            m.output(),
        ))
    }

    async fn call(
        &self,
        method: &str,
        headers: &HeaderMap,
        message: &[u8],
    ) -> Result<cs::Output, open_runo_connect_core::Status> {
        match AdminGrpc::call(self, method, headers, message).await {
            Err(e) => Err(open_runo_connect_core::Status::new(e.code, e.message)),
            Ok(Output { reply, headers }) => Ok(cs::Output {
                reply: match reply {
                    Reply::Unary(b) => cs::Reply::Unary(b),
                    Reply::Stream(s) => cs::Reply::Stream(s),
                },
                headers: headers
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), v))
                    .collect(),
            }),
        }
    }
}

/// 素のgRPC(HTTP/2)の1要求。`grpc::serve_grpc_with`から呼ばれる。
pub async fn handle_grpc(admin: &AdminGrpc, req: Request) -> Response {
    cs::handle(admin, req).await
}

/// gRPC-web用のHTTPハンドラ。`POST /open_runo.admin.v1.Admin/:method`に載せる。
pub fn grpc_web_handler(admin: AdminGrpc) -> crate::hyper_compat::Handler {
    browser_handler(admin)
}

/// ブラウザ向けの入口。同じパスに載せ、Content-Typeで gRPC-web[-text]・Connect・素のgRPCを
/// 振り分ける(1つのポートで全部受ける)。
pub fn browser_handler(admin: AdminGrpc) -> crate::hyper_compat::Handler {
    Arc::new(move |req, _params| {
        let admin = admin.clone();
        Box::pin(async move { cs::handle(&admin, req).await })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use crate::keyring::GuardianConfig;

    fn admin() -> (AdminGrpc, Arc<AppState>) {
        let state = Arc::new(AppState::new());
        let guardian = Arc::new(KeyGuardian::new(
            Arc::clone(&state.db),
            GuardianConfig::from_env(),
        ));
        (AdminGrpc::new(Arc::clone(&state), guardian), state)
    }

    fn key_headers(key: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("x-api-key", key.parse().unwrap());
        h
    }

    #[test]
    fn method_of_parses_only_the_admin_service() {
        assert_eq!(
            method_of("/open_runo.admin.v1.Admin/DbList"),
            Some("DbList")
        );
        assert_eq!(method_of("/open_runo.admin.v1.Admin/"), None);
        assert_eq!(method_of("/grpc.health.v1.Health/Check"), None);
        assert_eq!(method_of("/open_runo.admin.v1.Admin/a/b"), None);
    }

    #[tokio::test]
    async fn unauthenticated_calls_are_rejected_but_self_issue_is_open() {
        let (a, _s) = admin();
        // 不明メソッドは認証後にUNIMPLEMENTED(鍵レジストリが空の間は任意の鍵で通る)。
        let e = a
            .call("Nope", &key_headers("dev"), &[])
            .await
            .err()
            .unwrap();
        assert_eq!(e.code, code::UNIMPLEMENTED);
        let e = a
            .call("DbStatus", &HeaderMap::new(), &[])
            .await
            .err()
            .unwrap();
        assert_eq!(e.code, code::UNAUTHENTICATED);
        assert!(a.call("SelfIssueKey", &HeaderMap::new(), &[]).await.is_ok());
    }

    #[tokio::test]
    async fn schema_flow_and_error_codes() {
        let (a, _s) = admin();
        let h = key_headers("dev");
        let req = v1::RegisterSchemaRequest {
            service_name: "users".into(),
            sdl: "type Query { a: Int }".into(),
            stage: Some("staging".into()),
            namespace: None,
        }
        .encode_to_vec();
        let out = a.call("RegisterSchema", &h, &req).await.ok().unwrap();
        let Reply::Unary(b) = out.reply else {
            panic!("unary")
        };
        let v = v1::SchemaVersion::decode(&b[..]).unwrap();
        assert_eq!(
            (
                v.service_name.as_str(),
                v.stage.as_str(),
                v.namespace.as_str()
            ),
            ("users", "staging", "default")
        );

        let missing = v1::GetSchemaRequest {
            service_name: "nope".into(),
            stage: None,
            namespace: None,
        }
        .encode_to_vec();
        assert_eq!(
            a.call("GetSchema", &h, &missing).await.err().unwrap().code,
            code::NOT_FOUND
        );

        // 不正なフラグ値はINVALID_ARGUMENT、重複SCIMはALREADY_EXISTS。
        let bad = v1::UpsertFeatureFlagRequest {
            name: "x".into(),
            enabled: None,
            rollout_percent: Some(101),
            description: None,
        }
        .encode_to_vec();
        assert_eq!(
            a.call("UpsertFeatureFlag", &h, &bad)
                .await
                .err()
                .unwrap()
                .code,
            code::INVALID_ARGUMENT
        );
    }

    #[tokio::test]
    async fn watch_events_streams_a_registered_schema() {
        let (a, state) = admin();
        let h = key_headers("dev");
        let out = a.call("WatchEvents", &h, &[]).await.ok().unwrap();
        let Reply::Stream(mut s) = out.reply else {
            panic!("stream")
        };
        let _ = state.events.send(crate::state::SchemaEvent {
            service_name: "evt".into(),
            stage: "local".into(),
            at: "t".into(),
        });
        let first = tokio::time::timeout(std::time::Duration::from_secs(3), s.next())
            .await
            .expect("timed out")
            .expect("item");
        let ev = v1::AdminEvent::decode(&first[..]).unwrap();
        assert!(
            matches!(ev.event, Some(v1::admin_event::Event::Schema(s)) if s.service_name == "evt")
        );
    }

    // ── 実HTTP/2・実gRPC-webでの通し試験 ──

    use open_runo_proto::client::{ClientConfig, GrpcClient};

    async fn serve_h2c() -> (GrpcClient, Arc<AppState>, tokio::task::JoinHandle<()>) {
        let (a, state) = admin();
        let (addr, h) = crate::grpc::serve_grpc_with("127.0.0.1:0".parse().unwrap(), Some(a))
            .await
            .unwrap();
        let client = GrpcClient::new(ClientConfig {
            endpoint: format!("http://{addr}"),
            api_key: Some("dev".into()),
            ..Default::default()
        });
        (client, state, h)
    }

    #[tokio::test]
    async fn real_http2_unary_flow_and_status_codes() {
        let (c, _state, _h) = serve_h2c().await;
        let v: v1::SchemaVersion = c
            .unary(
                "RegisterSchema",
                &v1::RegisterSchemaRequest {
                    service_name: "users".into(),
                    sdl: "type Query { a: Int }".into(),
                    stage: None,
                    namespace: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(v.stage, "local");
        let h: v1::SchemaHistoryResponse = c
            .unary(
                "SchemaHistory",
                &v1::SchemaHistoryRequest {
                    service_name: "users".into(),
                    namespace: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(h.versions.len(), 1);

        // エラーはgrpc-statusで返る。
        let e = c
            .unary::<_, v1::SchemaVersion>(
                "GetSchema",
                &v1::GetSchemaRequest {
                    service_name: "nope".into(),
                    stage: None,
                    namespace: None,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(e.code, code::NOT_FOUND);
        // 鍵なしはUNAUTHENTICATED。
        let anon = c.with_api_key(None);
        let e = anon
            .unary::<_, open_runo_proto::prost_types::Struct>("DbStatus", &v1::Empty {})
            .await
            .unwrap_err();
        assert_eq!(e.code, code::UNAUTHENTICATED);
        // Structの応答はJSONへ戻せる(件数は整数のまま)。
        let s: open_runo_proto::prost_types::Struct =
            c.unary("DbStatus", &v1::Empty {}).await.unwrap();
        assert_eq!(open_runo_proto::struct_to_json(&s)["backend"], "in-memory");
    }

    #[tokio::test]
    async fn real_http2_server_streaming_delivers_events() {
        let (c, state, _h) = serve_h2c().await;
        let tx = state.events.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            let _ = tx.send(crate::state::SchemaEvent {
                service_name: "evt".into(),
                stage: "local".into(),
                at: "t".into(),
            });
        });
        let evs: Vec<v1::AdminEvent> = c
            .stream_some(
                "WatchEvents",
                &v1::Empty {},
                1,
                std::time::Duration::from_secs(5),
            )
            .await
            .unwrap();
        assert_eq!(evs.len(), 1);
        assert!(
            matches!(&evs[0].event, Some(v1::admin_event::Event::Schema(s)) if s.service_name == "evt")
        );
    }

    #[tokio::test]
    async fn grpc_web_over_plain_http_matches_grpc() {
        use crate::hyper_compat::{serve, Router};
        let (a, _state) = admin();
        let app = Router::new().route(
            Method::POST,
            "/open_runo.admin.v1.Admin/:method",
            grpc_web_handler(a),
        );
        let (addr, _h) = serve(app, "127.0.0.1:0".parse().unwrap()).await.unwrap();
        let call = |method: &str, msg: Vec<u8>, key: Option<&str>| {
            let mut r = reqwest::Client::new()
                .post(format!("http://{addr}/open_runo.admin.v1.Admin/{method}"))
                .header("content-type", "application/grpc-web+proto")
                .body(open_runo_proto::encode_frame(&msg));
            if let Some(k) = key {
                r = r.header("x-api-key", k);
            }
            async move { r.send().await.unwrap() }
        };
        let resp = call("SelfIssueKey", v1::Empty {}.encode_to_vec(), None).await;
        assert_eq!(resp.headers()["content-type"], "application/grpc-web+proto");
        let body = resp.bytes().await.unwrap();
        let (flag, msg, rest) = open_runo_proto::split_frame(&body).unwrap();
        assert_eq!(flag, 0);
        let issued = v1::SelfIssued::decode(msg).unwrap();
        assert!(issued.api_key.starts_with("orn_"));
        let (tflag, tbody, _) = open_runo_proto::split_frame(rest).unwrap();
        assert_eq!(tflag, 0x80);
        assert_eq!(open_runo_proto::parse_web_trailers(tbody).0, 0);

        // エラーもトレーラーフレームで返る(HTTPは200)。
        let resp = call("DbStatus", v1::Empty {}.encode_to_vec(), None).await;
        assert_eq!(resp.status(), 200);
        let body = resp.bytes().await.unwrap();
        let (tflag, tbody, _) = open_runo_proto::split_frame(&body).unwrap();
        assert_eq!(tflag, 0x80);
        assert_eq!(
            open_runo_proto::parse_web_trailers(tbody).0,
            code::UNAUTHENTICATED
        );

        // 鍵を付ければ通る。
        let resp = call(
            "DbStatus",
            v1::Empty {}.encode_to_vec(),
            Some(&issued.api_key),
        )
        .await;
        let body = resp.bytes().await.unwrap();
        let (_, msg, _) = open_runo_proto::split_frame(&body).unwrap();
        let s = open_runo_proto::prost_types::Struct::decode(msg).unwrap();
        assert_eq!(open_runo_proto::struct_to_json(&s)["status"], "ok");
    }

    #[tokio::test]
    async fn reflection_lists_and_describes_the_admin_service() {
        let (c, _state, _h) = serve_h2c().await;
        // ServerReflectionRequest { list_services = 7 (string) } — 手書きprotobuf(grpc.rsと同じ流儀)
        let mut req = vec![0x3a, 0x01, b'*'];
        let (frames, _) = c
            .with_api_key(None)
            .call_raw_path(
                "/grpc.reflection.v1.ServerReflection/ServerReflectionInfo",
                &req,
            )
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&frames[0]).to_string();
        assert!(text.contains("open_runo.admin.v1.Admin"), "{text}");
        // file_containing_symbol = 4 (string)
        let sym = b"open_runo.admin.v1.Admin";
        req = vec![0x22, sym.len() as u8];
        req.extend_from_slice(sym);
        let (frames, _) = c
            .call_raw_path(
                "/grpc.reflection.v1.ServerReflection/ServerReflectionInfo",
                &req,
            )
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&frames[0]).to_string();
        assert!(
            text.contains("RegisterSchema") && text.contains("WatchEvents"),
            "{text}"
        );
    }

    #[test]
    fn text_body_decoder_accepts_concatenated_padded_chunks() {
        use base64::Engine as _;
        let e = base64::engine::general_purpose::STANDARD;
        let (a, b) = (e.encode(b"hello"), e.encode(b"world!!"));
        assert_eq!(
            decode_text_body(format!("{a}{b}").as_bytes()).unwrap(),
            b"helloworld!!"
        );
        assert_eq!(
            decode_text_body(format!("{a}\n{b}").as_bytes()).unwrap(),
            b"helloworld!!"
        );
        assert!(decode_text_body(b"@@@@").is_none());
    }

    #[tokio::test]
    async fn grpc_web_text_round_trips_like_binary() {
        use crate::hyper_compat::{serve, Router};
        use base64::Engine as _;
        let e64 = base64::engine::general_purpose::STANDARD;
        let (a, _state) = admin();
        let app = Router::new().route(
            Method::POST,
            "/open_runo.admin.v1.Admin/:method",
            grpc_web_handler(a),
        );
        let (addr, _h) = serve(app, "127.0.0.1:0".parse().unwrap()).await.unwrap();
        let url = |m: &str| format!("http://{addr}/open_runo.admin.v1.Admin/{m}");
        let post = |m: &str, msg: &[u8], key: Option<&str>| {
            let mut r = reqwest::Client::new()
                .post(url(m))
                .header("content-type", "application/grpc-web-text")
                .header("accept", "application/grpc-web-text")
                .body(e64.encode(open_runo_proto::encode_frame(msg)));
            if let Some(k) = key {
                r = r.header("x-api-key", k);
            }
            async move { r.send().await.unwrap() }
        };
        // 応答は base64(データフレーム)+base64(トレーラーフレーム)の連結。
        let resp = post("SelfIssueKey", &v1::Empty {}.encode_to_vec(), None).await;
        assert_eq!(
            resp.headers()["content-type"],
            "application/grpc-web-text+proto"
        );
        let raw = decode_text_body(&resp.bytes().await.unwrap()).unwrap();
        let (flag, msg, rest) = open_runo_proto::split_frame(&raw).unwrap();
        assert_eq!(flag, 0);
        let issued = v1::SelfIssued::decode(msg).unwrap();
        let (tflag, tbody, _) = open_runo_proto::split_frame(rest).unwrap();
        assert_eq!(tflag, 0x80);
        assert_eq!(open_runo_proto::parse_web_trailers(tbody).0, 0);

        // エラーもbase64のトレーラーで返る。
        let resp = post("DbStatus", &v1::Empty {}.encode_to_vec(), None).await;
        let raw = decode_text_body(&resp.bytes().await.unwrap()).unwrap();
        let (_, tbody, _) = open_runo_proto::split_frame(&raw).unwrap();
        assert_eq!(
            open_runo_proto::parse_web_trailers(tbody).0,
            code::UNAUTHENTICATED
        );

        // 鍵付きなら通る。壊れたbase64はINVALID_ARGUMENT。
        let resp = post(
            "DbStatus",
            &v1::Empty {}.encode_to_vec(),
            Some(&issued.api_key),
        )
        .await;
        let raw = decode_text_body(&resp.bytes().await.unwrap()).unwrap();
        let (_, msg, _) = open_runo_proto::split_frame(&raw).unwrap();
        let s = open_runo_proto::prost_types::Struct::decode(msg).unwrap();
        assert_eq!(open_runo_proto::struct_to_json(&s)["status"], "ok");
        let bad = reqwest::Client::new()
            .post(url("DbStatus"))
            .header("content-type", "application/grpc-web-text")
            .body("@@@@")
            .send()
            .await
            .unwrap();
        let raw = decode_text_body(&bad.bytes().await.unwrap()).unwrap();
        let (_, tbody, _) = open_runo_proto::split_frame(&raw).unwrap();
        assert_eq!(
            open_runo_proto::parse_web_trailers(tbody).0,
            code::INVALID_ARGUMENT
        );
    }

    // ── TLS 1.3 / mTLS(`tls` featureが有効なときだけ) ──
    #[cfg(feature = "tls")]
    mod tls_tests {
        use super::*;

        #[test]
        fn ca_and_leaf_helpers_are_consistent() {
            let t = tls_material();
            assert!(t.ca_pem.contains("BEGIN CERTIFICATE"));
        }

        struct TlsMaterial {
            ca_pem: String,
            server_cert: String,
            server_key: String,
            client_cert: String,
            client_key: String,
        }

        fn tls_material() -> TlsMaterial {
            use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair};
            let ca_key = KeyPair::generate().unwrap();
            let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
            ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
            let ca_cert = ca_params.self_signed(&ca_key).unwrap();
            let issuer = Issuer::new(ca_params, ca_key);

            let server_key = KeyPair::generate().unwrap();
            let server = CertificateParams::new(vec!["localhost".to_string()])
                .unwrap()
                .signed_by(&server_key, &issuer)
                .unwrap();
            let client_key = KeyPair::generate().unwrap();
            let client = CertificateParams::new(vec!["admin-client".to_string()])
                .unwrap()
                .signed_by(&client_key, &issuer)
                .unwrap();
            TlsMaterial {
                ca_pem: ca_cert.pem(),
                server_cert: server.pem(),
                server_key: server_key.serialize_pem(),
                client_cert: client.pem(),
                client_key: client_key.serialize_pem(),
            }
        }

        async fn serve_tls(mtls: bool) -> (std::net::SocketAddr, TlsMaterial, Arc<AppState>) {
            let t = tls_material();
            let (a, state) = admin();
            let cfg = crate::grpc::grpc_tls_config(
                t.server_cert.as_bytes(),
                t.server_key.as_bytes(),
                mtls.then(|| t.ca_pem.as_bytes()),
            )
            .unwrap();
            let (addr, h) = crate::grpc::serve_grpc_tls(
                "127.0.0.1:0".parse().unwrap(),
                Some(a),
                cfg,
                crate::grpc::GrpcOptions::default(),
            )
            .await
            .unwrap();
            std::mem::forget(h);
            (addr, t, state)
        }

        fn tls_client(
            addr: std::net::SocketAddr,
            t: &TlsMaterial,
            with_cert: bool,
            with_ca: bool,
        ) -> GrpcClient {
            GrpcClient::new(ClientConfig {
                endpoint: format!("https://localhost:{}", addr.port()),
                api_key: Some("dev".into()),
                ca_pem: with_ca.then(|| t.ca_pem.clone().into_bytes()),
                client_cert_pem: with_cert.then(|| t.client_cert.clone().into_bytes()),
                client_key_pem: with_cert.then(|| t.client_key.clone().into_bytes()),
                ..Default::default()
            })
        }

        #[tokio::test]
        async fn grpc_over_tls13_works_and_untrusted_servers_are_rejected() {
            let (addr, t, _s) = serve_tls(false).await;
            let ok: open_runo_proto::prost_types::Struct = tls_client(addr, &t, false, true)
                .unary("DbStatus", &v1::Empty {})
                .await
                .unwrap();
            assert_eq!(open_runo_proto::struct_to_json(&ok)["status"], "ok");
            // 自己発行CAを信頼していないクライアント(WebPKIルートのみ)は接続できない。
            let e = tls_client(addr, &t, false, false)
                .unary::<_, open_runo_proto::prost_types::Struct>("DbStatus", &v1::Empty {})
                .await
                .unwrap_err();
            assert_eq!(e.code, 14, "{e}");
        }

        #[tokio::test]
        async fn tls12_clients_are_refused() {
            use tokio_rustls::rustls::{self, pki_types::ServerName, RootCertStore};
            let (addr, t, _s) = serve_tls(false).await;
            let mut roots = RootCertStore::empty();
            for c in rustls_pemfile::certs(&mut t.ca_pem.as_bytes()) {
                roots.add(c.unwrap()).unwrap();
            }
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let tls12 = rustls::ClientConfig::builder_with_provider(Arc::clone(&provider))
                .with_protocol_versions(&[&rustls::version::TLS12])
                .unwrap()
                .with_root_certificates(roots.clone())
                .with_no_client_auth();
            let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
            let r = tokio_rustls::TlsConnector::from(Arc::new(tls12))
                .connect(ServerName::try_from("localhost").unwrap(), tcp)
                .await;
            assert!(
                r.is_err(),
                "a TLS1.2-only client must not complete the handshake"
            );
            // 対照: TLS1.3なら同じサーバーにつながる(テスト自体が壊れていないことの確認)。
            let tls13 = rustls::ClientConfig::builder_with_provider(provider)
                .with_protocol_versions(&[&rustls::version::TLS13])
                .unwrap()
                .with_root_certificates(roots)
                .with_no_client_auth();
            let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
            let ok = tokio_rustls::TlsConnector::from(Arc::new(tls13))
                .connect(ServerName::try_from("localhost").unwrap(), tcp)
                .await;
            assert!(ok.is_ok());
        }

        #[tokio::test]
        async fn mtls_requires_a_client_certificate_signed_by_the_client_ca() {
            let (addr, t, _s) = serve_tls(true).await;
            // クライアント証明書なし → 拒否(TLS 1.3ではハンドシェイク完了後の最初の通信で切られる)。
            let e = tls_client(addr, &t, false, true)
                .unary::<_, open_runo_proto::prost_types::Struct>("DbStatus", &v1::Empty {})
                .await;
            assert!(e.is_err(), "no client cert must be refused");
            // 正しいクライアント証明書 → 通る(呼び出しの認可は別途x-api-key)。
            let ok: open_runo_proto::prost_types::Struct = tls_client(addr, &t, true, true)
                .unary("DbStatus", &v1::Empty {})
                .await
                .unwrap();
            assert_eq!(open_runo_proto::struct_to_json(&ok)["status"], "ok");
            // mTLSを通っても、鍵なしの呼び出しは認可されない(二重の守り)。
            let anon = tls_client(addr, &t, true, true).with_api_key(None);
            let e = anon
                .unary::<_, open_runo_proto::prost_types::Struct>("DbStatus", &v1::Empty {})
                .await
                .unwrap_err();
            assert_eq!(e.code, code::UNAUTHENTICATED);
        }
    }

    // ── リフレクションの無効化 ──
    #[tokio::test]
    async fn reflection_can_be_turned_off_for_production() {
        let (a, _state) = admin();
        let (addr, _h) = crate::grpc::serve_grpc_opts(
            "127.0.0.1:0".parse().unwrap(),
            Some(a),
            crate::grpc::GrpcOptions { reflection: false },
        )
        .await
        .unwrap();
        let c = GrpcClient::new(ClientConfig {
            endpoint: format!("http://{addr}"),
            ..Default::default()
        });
        let e = c
            .call_raw_path(
                "/grpc.reflection.v1.ServerReflection/ServerReflectionInfo",
                &[0x3a, 0x01, b'*'],
            )
            .await
            .unwrap_err();
        assert_eq!(e.code, code::UNIMPLEMENTED);
        // 管理サービス自体は動く。
        let k: v1::SelfIssued = c.unary("SelfIssueKey", &v1::Empty {}).await.unwrap();
        assert!(k.api_key.starts_with("orn_"));
    }

    // ── 管理操作の通し試験(旧GraphQL/RESTの管理試験の期待値を、gRPCの`call`へ移したもの) ──

    use open_runo_proto::prost_types::Struct;

    async fn unary<M: Message + Default>(
        a: &AdminGrpc,
        method: &str,
        h: &HeaderMap,
        req: &impl Message,
    ) -> M {
        match a.call(method, h, &req.encode_to_vec()).await {
            Ok(Output {
                reply: Reply::Unary(b),
                ..
            }) => M::decode(&b[..]).unwrap(),
            Ok(_) => panic!("{method}: not unary"),
            Err(e) => panic!("{method}: {e:?}"),
        }
    }

    async fn status<R: Message>(a: &AdminGrpc, method: &str, h: &HeaderMap, req: &R) -> i32 {
        a.call(method, h, &req.encode_to_vec())
            .await
            .err()
            .unwrap_or_else(|| panic!("{method} should fail"))
            .code
    }

    fn js(s: &Struct) -> serde_json::Value {
        open_runo_proto::struct_to_json(s)
    }

    #[tokio::test]
    async fn federation_compose_and_status() {
        let (a, _s) = admin();
        let h = key_headers("dev");
        let svc = |name: &str, types: &[(&str, &[&str])]| v1::ComposeService {
            service_name: name.into(),
            types: types
                .iter()
                .map(|(t, f)| {
                    (
                        (*t).to_string(),
                        v1::FieldList {
                            fields: f.iter().map(|x| x.to_string()).collect(),
                        },
                    )
                })
                .collect(),
            sdl: None,
        };
        let c: v1::ComposeResponse = unary(
            &a,
            "ComposeFederation",
            &h,
            &v1::ComposeRequest {
                services: vec![
                    svc("a", &[("User", &["id", "name"])]),
                    svc("b", &[("User", &["id", "email"]), ("Post", &["id"])]),
                ],
            },
        )
        .await;
        assert_eq!(c.contributing_services, vec!["a", "b"]);
        assert!(c.breaking_changes.is_empty());
        assert_eq!(c.types["User"].fields, vec!["email", "id", "name"]);
        let st: v1::FederationStatusResponse =
            unary(&a, "FederationStatus", &h, &v1::Empty {}).await;
        assert_eq!((st.type_count, st.field_count), (2, 4));
        // 型が消えれば破壊的変更。不正SDLはINVALID_ARGUMENT。
        let c2: v1::ComposeResponse = unary(
            &a,
            "ComposeFederation",
            &h,
            &v1::ComposeRequest {
                services: vec![svc("a", &[("User", &["id"])])],
            },
        )
        .await;
        assert!(!c2.breaking_changes.is_empty());
        let bad = v1::ComposeRequest {
            services: vec![v1::ComposeService {
                service_name: "x".into(),
                types: Default::default(),
                sdl: Some("type {{{".into()),
            }],
        };
        assert_eq!(
            status(&a, "ComposeFederation", &h, &bad).await,
            code::INVALID_ARGUMENT
        );
    }

    #[tokio::test]
    async fn feature_flags_persisted_queries_and_cache() {
        let (a, _s) = admin();
        let h = key_headers("dev");
        let up = |name: &str, rollout: u32| v1::UpsertFeatureFlagRequest {
            name: name.into(),
            enabled: None,
            rollout_percent: Some(rollout),
            description: Some("d".into()),
        };
        let f: v1::FeatureFlag = unary(&a, "UpsertFeatureFlag", &h, &up("canary", 100)).await;
        assert_eq!(
            (f.name.as_str(), f.rollout_percent, f.enabled),
            ("canary", 100, true)
        );
        let _: v1::FeatureFlag = unary(&a, "UpsertFeatureFlag", &h, &up("off", 0)).await;
        let l: v1::ListFeatureFlagsResponse =
            unary(&a, "ListFeatureFlags", &h, &v1::Empty {}).await;
        assert_eq!(l.flags.len(), 2);
        let ev = |n: &str, k: &str| v1::EvaluateFlagRequest {
            name: n.into(),
            bucket_key: k.into(),
        };
        for k in ["u1", "u2", "u3"] {
            let e: v1::FlagEvaluation =
                unary(&a, "EvaluateFeatureFlag", &h, &ev("canary", k)).await;
            assert!(e.enabled);
        }
        let e: v1::FlagEvaluation = unary(&a, "EvaluateFeatureFlag", &h, &ev("off", "u1")).await;
        assert!(!e.enabled);
        assert_eq!(
            status(
                &a,
                "GetFeatureFlag",
                &h,
                &v1::FlagName {
                    name: "none".into()
                }
            )
            .await,
            code::NOT_FOUND
        );
        assert_eq!(
            status(&a, "UpsertFeatureFlag", &h, &up("bad", 101)).await,
            code::INVALID_ARGUMENT
        );
        let _: v1::Empty = unary(
            &a,
            "DeleteFeatureFlag",
            &h,
            &v1::FlagName {
                name: "canary".into(),
            },
        )
        .await;
        assert_eq!(
            status(
                &a,
                "DeleteFeatureFlag",
                &h,
                &v1::FlagName {
                    name: "canary".into()
                }
            )
            .await,
            code::NOT_FOUND
        );

        // 永続クエリ。
        let p: v1::PersistedQuery = unary(
            &a,
            "RegisterPersistedQuery",
            &h,
            &v1::RegisterPersistedQueryRequest {
                query: "{ health }".into(),
            },
        )
        .await;
        assert_eq!(p.hash.len(), 64);
        let g: v1::PersistedQuery = unary(
            &a,
            "GetPersistedQuery",
            &h,
            &v1::PersistedQueryHash { hash: p.hash },
        )
        .await;
        assert_eq!(g.query, "{ health }");
        assert_eq!(
            status(
                &a,
                "GetPersistedQuery",
                &h,
                &v1::PersistedQueryHash {
                    hash: "deadbeef".into()
                }
            )
            .await,
            code::NOT_FOUND
        );

        // キャッシュ消去。
        let r: v1::PurgeResponse = unary(
            &a,
            "PurgeCache",
            &h,
            &v1::PurgeRequest { path: "/a".into() },
        )
        .await;
        assert_eq!(r.purged, "/a");
        let r: v1::PurgeResponse = unary(&a, "PurgeAllCache", &h, &v1::Empty {}).await;
        assert_eq!(r.purged, "*");
    }

    #[tokio::test]
    async fn db_analytics_ai_route_and_backup() {
        let (a, state) = admin();
        let h = key_headers("dev");
        let s: Struct = unary(&a, "DbStatus", &h, &v1::Empty {}).await;
        assert_eq!(
            js(&s),
            serde_json::json!({ "backend": "in-memory", "status": "ok" })
        );
        let r: Struct = unary(&a, "DbRouting", &h, &v1::Empty {}).await;
        assert_eq!(js(&r)["entries"].as_array().unwrap().len(), 9);
        let put = v1::DbPutRequest {
            table: "notes".into(),
            key: "k1".into(),
            value: Some(open_runo_proto::json_to_value(
                &serde_json::json!({ "n": 1, "tags": ["a", "b"] }),
            )),
        };
        let p: Struct = unary(&a, "DbPut", &h, &put).await;
        assert_eq!(
            js(&p)["value"],
            serde_json::json!({ "n": 1, "tags": ["a", "b"] })
        );
        let key = v1::DbKey {
            table: "notes".into(),
            key: "k1".into(),
        };
        let g: Struct = unary(&a, "DbGet", &h, &key).await;
        assert_eq!(js(&g)["key"], "k1");
        let l: Struct = unary(
            &a,
            "DbList",
            &h,
            &v1::DbTable {
                table: "notes".into(),
            },
        )
        .await;
        assert_eq!(js(&l)["count"], 1);
        assert_eq!(
            status(
                &a,
                "DbGetAtCommit",
                &h,
                &v1::DbAtCommit {
                    table: "notes".into(),
                    key: "k1".into(),
                    commit_id: "abc".into()
                }
            )
            .await,
            code::UNIMPLEMENTED
        );
        let d: Struct = unary(&a, "DbDelete", &h, &key).await;
        assert_eq!(js(&d)["deleted"], true);
        assert_eq!(status(&a, "DbGet", &h, &key).await, code::NOT_FOUND);

        // 分析とAI経路。
        let m: Struct = unary(&a, "AnalyticsRequestsPerMonth", &h, &v1::Empty {}).await;
        assert!(js(&m)["months"].is_array());
        let cand = |p: &str, cost: f64, local: bool, ctx: u32| v1::RouteCandidate {
            provider: p.into(),
            estimated_cost_usd_per_1k_tokens: cost,
            estimated_latency_ms: 300,
            is_local: local,
            context_length: ctx,
        };
        let route: Struct = unary(
            &a,
            "AiRoute",
            &h,
            &v1::AiRouteRequest {
                policy: "cost".into(),
                min_context_length: Some(8000),
                candidates: vec![
                    cand("openai", 0.5, false, 128000),
                    cand("local", 0.0, true, 32000),
                ],
            },
        )
        .await;
        assert_eq!(js(&route)["selected_provider"], "local_llm");
        let none = v1::AiRouteRequest {
            policy: "cost".into(),
            min_context_length: Some(999_999),
            candidates: vec![cand("openai", 0.5, false, 1000)],
        };
        assert_eq!(
            status(&a, "AiRoute", &h, &none).await,
            code::INVALID_ARGUMENT
        );

        // バックアップ・移行・整合性。
        state.db.put("notes", "k", "{\"v\":1}").await.unwrap();
        let i: Struct = unary(&a, "IntegrityCheck", &h, &v1::Empty {}).await;
        assert_eq!(js(&i)["healed"], 0);
        let b: Struct = unary(&a, "BackupExport", &h, &v1::Empty {}).await;
        let path = js(&b)["written"][0].as_str().unwrap().to_string();
        let imp: Struct = unary(&a, "BackupImport", &h, &v1::BackupImportRequest { path }).await;
        assert!(js(&imp)["restored"].as_u64().unwrap() >= 1);
        assert_eq!(
            status(
                &a,
                "BackupImport",
                &h,
                &v1::BackupImportRequest {
                    path: "/nonexistent/x.json".into()
                }
            )
            .await,
            code::INVALID_ARGUMENT
        );
        let sql: Struct = unary(
            &a,
            "MigrateExportSql",
            &h,
            &v1::MigrateExportSqlRequest {
                dialect: "postgres".into(),
            },
        )
        .await;
        assert!(!js(&sql)["written"].as_array().unwrap().is_empty());
        assert_eq!(
            status(
                &a,
                "MigrateExportSql",
                &h,
                &v1::MigrateExportSqlRequest {
                    dialect: "oracle".into()
                }
            )
            .await,
            code::INVALID_ARGUMENT
        );
        let csv: Struct = unary(&a, "MigrateExportCsv", &h, &v1::Empty {}).await;
        assert!(!js(&csv)["written"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn scim_and_session_flow() {
        let (a, _s) = admin();
        let h = key_headers("dev");
        let c: Struct = unary(
            &a,
            "ScimCreateUser",
            &h,
            &v1::ScimCreateUserRequest {
                user_name: "alice@example.com".into(),
                roles: vec!["developer".into()],
            },
        )
        .await;
        let c = js(&c);
        let key = c["urn:open-runo:params:scim:api-key"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(key.starts_with("orn_"));
        let id = c["id"].as_str().unwrap().to_string();
        // 鍵レジストリが空でなくなったので、以後は発行された鍵で呼ぶ。
        let h = key_headers(&key);
        let l: Struct = unary(
            &a,
            "ScimListUsers",
            &h,
            &v1::ScimListRequest { filter: None },
        )
        .await;
        assert_eq!(js(&l)["totalResults"], 1);
        let dup = v1::ScimCreateUserRequest {
            user_name: "alice@example.com".into(),
            roles: vec![],
        };
        assert_eq!(
            status(&a, "ScimCreateUser", &h, &dup).await,
            code::ALREADY_EXISTS
        );

        // セッション: x-api-keyをCookie+CSRFへ交換し、set-cookieが返る。そのCookie+CSRFで呼べる。
        let out = a.call("SessionLogin", &h, &[]).await.ok().unwrap();
        let cookie = out
            .headers
            .iter()
            .find(|(k, _)| *k == "set-cookie")
            .unwrap()
            .1
            .clone();
        let Reply::Unary(b) = out.reply else {
            panic!("unary")
        };
        let login = v1::SessionLoginResponse::decode(&b[..]).unwrap();
        let mut sh = HeaderMap::new();
        sh.insert("cookie", cookie.split(';').next().unwrap().parse().unwrap());
        // CSRFなしは拒否(POST相当)。あれば通る。
        assert_eq!(
            status(
                &a,
                "ScimListUsers",
                &sh,
                &v1::ScimListRequest { filter: None }
            )
            .await,
            code::PERMISSION_DENIED
        );
        sh.insert("x-csrf-token", login.csrf_token.parse().unwrap());
        let _: Struct = unary(
            &a,
            "ScimListUsers",
            &sh,
            &v1::ScimListRequest { filter: None },
        )
        .await;
        let out = a.call("SessionLogout", &sh, &[]).await.ok().unwrap();
        assert!(out.headers.iter().any(|(k, _)| *k == "set-cookie"));
        assert_eq!(
            status(
                &a,
                "ScimListUsers",
                &sh,
                &v1::ScimListRequest { filter: None }
            )
            .await,
            code::UNAUTHENTICATED
        );

        // ユーザー削除で鍵が失効する。
        let _: v1::Empty = unary(&a, "ScimDeleteUser", &h, &v1::ScimUserId { id }).await;
        assert_eq!(
            status(
                &a,
                "ScimListUsers",
                &h,
                &v1::ScimListRequest { filter: None }
            )
            .await,
            code::UNAUTHENTICATED
        );
    }

    /// 同じポートで、HTTP/1.1(curl・Connect JSON)と素のgRPC(HTTP/2 h2c)の両方が動く。
    #[tokio::test]
    async fn one_port_serves_http1_curl_and_plain_grpc() {
        use crate::hyper_compat::{serve, Router};
        let (a, _state) = admin();
        let app = Router::new().route(
            Method::POST,
            "/open_runo.admin.v1.Admin/:method",
            browser_handler(a),
        );
        let (addr, _h) = serve(app, "127.0.0.1:0".parse().unwrap()).await.unwrap();
        // HTTP/1.1: curl相当のConnect JSON。
        let r = reqwest::Client::new()
            .post(format!(
                "http://{addr}/open_runo.admin.v1.Admin/SelfIssueKey"
            ))
            .header("content-type", "application/json")
            .body("{}")
            .send()
            .await
            .unwrap();
        assert_eq!(r.version(), reqwest::Version::HTTP_11);
        let key = r.json::<serde_json::Value>().await.unwrap()["apiKey"]
            .as_str()
            .unwrap()
            .to_string();
        // HTTP/2(h2c): 素のgRPCクライアント(CLIと同じ)が**同じポート**で通る。
        let c = GrpcClient::new(ClientConfig {
            endpoint: format!("http://{addr}"),
            api_key: Some(key),
            ..Default::default()
        });
        let s: open_runo_proto::prost_types::Struct =
            c.unary("DbStatus", &v1::Empty {}).await.unwrap();
        assert_eq!(open_runo_proto::struct_to_json(&s)["status"], "ok");
        let e = c
            .with_api_key(Some("bogus".into()))
            .unary::<_, open_runo_proto::prost_types::Struct>("DbStatus", &v1::Empty {})
            .await
            .unwrap_err();
        assert_eq!(e.code, code::UNAUTHENTICATED);
    }
}
