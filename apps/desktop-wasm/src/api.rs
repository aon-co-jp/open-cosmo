//! open-runo 管理サービス(`open_runo.admin.v1.Admin`)のブラウザ(WASM)クライアント。
//!
//! **ハイブリッド通信**(ConnectとgRPC-webの良いとこ取り):
//! - 単項呼び出しは**Connect**(`application/proto`の素の本文)。枠もトレーラーも無いので軽く、
//!   エラーは**HTTPステータス**(401/404/…)として開発者ツールのNetworkにそのまま見える。
//!   `curl`で同じ要求を再現できる(JSON版もサーバーが受ける)。
//! - ストリーミングは**Connectのエンベロープ**を`ReadableStream`で読む(サーバーストリーミング、
//!   `AbortController`で中断、複数の呼び出しを同時に走らせても互いに待たない)。
//! - Connectを知らないサーバー(gRPC-webだけ話す古い版)だと分かったら、**自動でgRPC-webへ切り替える**。
//! - すべての呼び出しを[`log_entries`]に記録する(管理画面の「RPC Inspector」と、`open-runo-debug`が
//!   `1`のときのブラウザコンソール出力に使う)。
//!
//! 型は`open-runo-proto`(`admin.proto`から生成)で、サーバー・CLIと共有する。公開関数の名前・引数・
//! 戻り値は従来と同じに保ち、応答は`open-runo-api-types`の型へデコードする(ページ側は無変更)。
//! 生存確認の `/health` だけは管理APIではないため従来どおりGETで叩く。

use open_runo_api_types::{
    DbRecordListResponse, DbRecordResponse, FeatureFlagEvaluationResponse, FeatureFlagListResponse,
    FeatureFlagResponse, FederationStatusResponse, SchemaHistoryResponse, SchemaVersion,
};
use open_runo_proto::prost::Message;
use open_runo_proto::{code, encode_frame, parse_web_trailers, split_frame, struct_to_json, v1};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cell::{Cell, RefCell};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{Request, RequestInit, RequestMode, Response};

/// `localStorage` key the self-issued API key is cached under, so a page
/// reload doesn't re-issue a fresh one every time.
const STORAGE_KEY: &str = "open-runo-api-key";

thread_local! {
    /// In-memory cache for this page load. WASM is single-threaded so a
    /// `RefCell` is enough — no need for a `Mutex`.
    static CACHED_KEY: RefCell<Option<String>> = const { RefCell::new(None) };
}

fn local_storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok()?
}

/// Drop the cached key (memory + `localStorage`) so the next call
/// transparently self-issues a fresh one.
fn clear_cached_api_key() {
    CACHED_KEY.with(|c| *c.borrow_mut() = None);
    if let Some(storage) = local_storage() {
        let _ = storage.remove_item(STORAGE_KEY);
    }
}

/// gRPCの失敗(コード+メッセージ)。HTTPレベルの失敗はコード`UNKNOWN`で文字列に入れる。
struct RpcError {
    code: i32,
    message: String,
}

/// 「このサーバーはConnectを話さない(gRPC-webだけ)」を表す内部用のコード(自動切替の合図)。
const NOT_CONNECT: i32 = -1;

impl RpcError {
    fn text(message: impl Into<String>) -> Self {
        Self {
            code: code::UNKNOWN,
            message: message.into(),
        }
    }
    fn into_string(self) -> String {
        if self.code == code::UNKNOWN || self.code == code::OK {
            self.message
        } else {
            format!("{}: {}", code::name(self.code), self.message)
        }
    }
}

/// 使う通信方式。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Wire {
    Connect,
    GrpcWeb,
}

impl Wire {
    pub fn name(self) -> &'static str {
        match self {
            Wire::Connect => "Connect",
            Wire::GrpcWeb => "gRPC-web",
        }
    }
}

/// RPC呼び出しの記録1件(RPC Inspector用)。
#[derive(Clone, Debug)]
pub struct LogEntry {
    pub at_ms: f64,
    pub method: String,
    pub wire: &'static str,
    pub http_status: u16,
    pub grpc_code: i32,
    pub ms: f64,
    pub request: String,
    pub response: String,
}

const LOG_CAPACITY: usize = 200;

thread_local! {
    static WIRE: Cell<Wire> = const { Cell::new(Wire::Connect) };
    static LOG: RefCell<Vec<LogEntry>> = const { RefCell::new(Vec::new()) };
}

pub fn current_wire() -> Wire {
    WIRE.with(Cell::get)
}

pub fn set_wire(w: Wire) {
    WIRE.with(|c| c.set(w));
}

pub fn log_entries() -> Vec<LogEntry> {
    LOG.with(|l| l.borrow().clone())
}

pub fn clear_log() {
    LOG.with(|l| l.borrow_mut().clear());
}

fn debug_enabled() -> bool {
    local_storage()
        .and_then(|s| s.get_item("open-runo-debug").ok().flatten())
        .is_some_and(|v| v == "1")
}

pub fn set_debug(on: bool) {
    if let Some(s) = local_storage() {
        let _ = if on { s.set_item("open-runo-debug", "1") } else { s.remove_item("open-runo-debug") };
    }
}

pub fn is_debug() -> bool {
    debug_enabled()
}

fn record(entry: LogEntry) {
    if debug_enabled() {
        // 開発者ツールのコンソールに、折りたたみグループで出す(Networkタブと突き合わせやすい)。
        let title = format!(
            "{} [{}] {} → HTTP {} / {} ({:.0} ms)",
            String::from(js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(entry.at_ms)).to_locale_time_string("ja-JP")),
            entry.wire,
            entry.method,
            entry.http_status,
            code::name(entry.grpc_code),
            entry.ms
        );
        web_sys::console::group_collapsed_1(&wasm_bindgen::JsValue::from_str(&title));
        web_sys::console::log_1(&wasm_bindgen::JsValue::from_str(&format!("request:  {}", entry.request)));
        web_sys::console::log_1(&wasm_bindgen::JsValue::from_str(&format!("response: {}", entry.response)));
        web_sys::console::group_end();
    }
    LOG.with(|l| {
        let mut l = l.borrow_mut();
        l.push(entry);
        if l.len() > LOG_CAPACITY {
            let drop = l.len() - LOG_CAPACITY;
            l.drain(..drop);
        }
        publish_to_devtools(&l);
    });
}

/// ブラウザ拡張の開発者ツールパネル(`apps/devtools-extension`)が読めるよう、記録をJSON文字列で
/// `window.__openRunoRpcLog`に置く。
fn publish_to_devtools(entries: &[LogEntry]) {
    let rows: Vec<serde_json::Value> = entries
        .iter()
        .map(|e| {
            serde_json::json!({
                "at": e.at_ms, "method": e.method, "wire": e.wire, "http": e.http_status,
                "grpc": e.grpc_code, "grpcName": code::name(e.grpc_code), "ms": e.ms,
                "request": e.request, "response": e.response,
            })
        })
        .collect();
    if let (Some(w), Ok(text)) = (web_sys::window(), serde_json::to_string(&rows)) {
        let _ = js_sys::Reflect::set(
            &w,
            &wasm_bindgen::JsValue::from_str("__openRunoRpcLog"),
            &wasm_bindgen::JsValue::from_str(&text),
        );
    }
}

/// `fetch`の結果。
struct Fetched {
    status: u16,
    ok: bool,
    request_id: Option<String>,
    content_type: String,
    grpc_status: Option<i32>,
    grpc_message: Option<String>,
    bytes: Vec<u8>,
}

/// `fetch`を1回行う。`content_type`を渡すとPOSTの本文型として付ける。
async fn fetch_bytes(
    method: &str,
    path: &str,
    body: Option<&[u8]>,
    api_key: Option<&str>,
    content_type: Option<&str>,
    extra: &[(&str, &str)],
) -> Result<Fetched, RpcError> {
    let js = |e: wasm_bindgen::JsValue| RpcError::text(format!("{e:?}"));
    let opts = RequestInit::new();
    opts.set_method(method);
    opts.set_mode(RequestMode::SameOrigin);
    if let Some(body) = body {
        opts.set_body(&js_sys::Uint8Array::from(body));
    }
    let request = Request::new_with_str_and_init(path, &opts).map_err(js)?;
    let headers = request.headers();
    if let Some(key) = api_key {
        headers.set("x-api-key", key).map_err(js)?;
    }
    if let Some(ct) = content_type {
        headers.set("content-type", ct).map_err(js)?;
    }
    for (k, v) in extra {
        headers.set(k, v).map_err(js)?;
    }
    let window = web_sys::window().ok_or_else(|| RpcError::text("no window"))?;
    let resp: Response = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|e| RpcError::text(format!("fetch error: {e:?}")))?
        .dyn_into()
        .map_err(js)?;
    let buf = JsFuture::from(resp.array_buffer().map_err(js)?)
        .await
        .map_err(|e| RpcError::text(format!("body read error: {e:?}")))?;
    let h = resp.headers();
    let get = |n: &str| h.get(n).ok().flatten();
    Ok(Fetched {
        status: resp.status(),
        ok: resp.ok(),
        request_id: get("x-request-id"),
        content_type: get("content-type").unwrap_or_default(),
        grpc_status: get("grpc-status").and_then(|v| v.parse().ok()),
        grpc_message: get("grpc-message").map(|m| open_runo_proto::percent_decode(&m)),
        bytes: js_sys::Uint8Array::new(&buf).to_vec(),
    })
}

/// HTTP自体の失敗(流量制限など)を文字列エラーにする。
fn http_failure(f: &Fetched) -> RpcError {
    let suffix = f.request_id.as_ref().map(|id| format!(" (request-id: {id})")).unwrap_or_default();
    RpcError::text(if f.status == 429 {
        format!("HTTP 429 (rate limited){suffix}")
    } else {
        format!("HTTP {}{suffix}", f.status)
    })
}

/// **Connect**の単項呼び出し1回(`application/proto`の素の本文)。HTTPステータスがそのまま結果を表す。
async fn call_connect(method: &str, req: &[u8], api_key: Option<&str>) -> Result<(Vec<u8>, u16), RpcError> {
    let f = fetch_bytes(
        "POST",
        &open_runo_proto::method_path(method),
        Some(req),
        api_key,
        Some("application/proto"),
        &[("connect-protocol-version", "1"), ("accept", "application/proto")],
    )
    .await?;
    // gRPC-webだけ話すサーバーは、gRPC-web形式でエラーを返す。→ 自動でそちらへ切り替える合図。
    if f.content_type.starts_with("application/grpc-web") {
        return Err(RpcError { code: NOT_CONNECT, message: String::new() });
    }
    if f.status == 200 {
        return Ok((f.bytes, f.status));
    }
    if f.status == 429 || f.content_type.is_empty() {
        return Err(http_failure(&f));
    }
    // Connectのエラー: HTTPステータス + JSON `{code, message}`。gRPCのコードはヘッダにも載る。
    let json: Value = serde_json::from_slice(&f.bytes).unwrap_or(Value::Null);
    let message = f
        .grpc_message
        .clone()
        .or_else(|| json["message"].as_str().map(str::to_string))
        .unwrap_or_default();
    let suffix = f.request_id.as_ref().map(|id| format!(" (request-id: {id})")).unwrap_or_default();
    Err(RpcError {
        code: f.grpc_status.unwrap_or(code::UNKNOWN),
        message: format!("{message}{suffix}"),
    })
}

/// **gRPC-web**の単項呼び出し1回(Connectを話さないサーバー向けのフォールバック)。
async fn call_grpc_web(method: &str, req: &[u8], api_key: Option<&str>) -> Result<(Vec<u8>, u16), RpcError> {
    let body = encode_frame(req);
    let f = fetch_bytes(
        "POST",
        &open_runo_proto::method_path(method),
        Some(&body),
        api_key,
        Some("application/grpc-web+proto"),
        &[("x-grpc-web", "1"), ("accept", "application/grpc-web+proto")],
    )
    .await?;
    if !f.ok {
        return Err(http_failure(&f));
    }
    let suffix = f.request_id.as_ref().map(|id| format!(" (request-id: {id})")).unwrap_or_default();
    // データフレーム(0個以上)の後ろに、フラグ0x80のトレーラーフレームが来る。
    let mut message = Vec::new();
    let mut status_pair = (code::UNKNOWN, "missing grpc-web trailers".to_string());
    let mut rest: &[u8] = &f.bytes;
    while let Some((flag, body, tail)) = split_frame(rest) {
        if flag & 0x80 != 0 {
            status_pair = parse_web_trailers(body);
        } else if message.is_empty() {
            message = body.to_vec();
        }
        rest = tail;
    }
    if status_pair.0 != code::OK {
        return Err(RpcError { code: status_pair.0, message: format!("{}{suffix}", status_pair.1) });
    }
    Ok((message, f.status))
}

/// 単項呼び出し1回。既定はConnect、Connectを話さないと分かれば以後はgRPC-webに切り替える。
async fn call_once<Req: Message>(method: &str, req: &Req, api_key: Option<&str>) -> Result<Vec<u8>, RpcError> {
    let bytes = req.encode_to_vec();
    let started = js_sys::Date::now();
    let wire_before = current_wire();
    let mut wire = wire_before;
    let mut result = match wire {
        Wire::Connect => call_connect(method, &bytes, api_key).await,
        Wire::GrpcWeb => call_grpc_web(method, &bytes, api_key).await,
    };
    if matches!(&result, Err(e) if e.code == NOT_CONNECT) {
        set_wire(Wire::GrpcWeb);
        wire = Wire::GrpcWeb;
        result = call_grpc_web(method, &bytes, api_key).await;
    }
    let (http_status, grpc_code, response) = match &result {
        Ok((b, st)) => (*st, code::OK, format!("{} bytes", b.len())),
        Err(e) => (0, e.code, e.message.clone()),
    };
    record(LogEntry {
        at_ms: started,
        method: method.to_string(),
        wire: wire.name(),
        http_status,
        grpc_code,
        ms: js_sys::Date::now() - started,
        request: format!("{} bytes", bytes.len()),
        response,
    });
    result.map(|(b, _)| b)
}

/// Get a working API key, obtaining one automatically if needed — the user
/// of the app never sees, enters, or configures a key. Order: in-memory
/// cache → `localStorage` (survives reloads) → `SelfIssueKey` RPC (認証不要)。
async fn get_or_issue_api_key() -> Result<String, RpcError> {
    if let Some(key) = CACHED_KEY.with(|c| c.borrow().clone()) {
        return Ok(key);
    }
    if let Some(storage) = local_storage() {
        if let Ok(Some(key)) = storage.get_item(STORAGE_KEY) {
            if !key.is_empty() {
                CACHED_KEY.with(|c| *c.borrow_mut() = Some(key.clone()));
                return Ok(key);
            }
        }
    }
    let bytes = call_once("SelfIssueKey", &v1::Empty {}, None).await.map_err(|e| RpcError {
        code: e.code,
        message: format!("self-issue failed: {}", e.message),
    })?;
    let issued = v1::SelfIssued::decode(&bytes[..])
        .map_err(|e| RpcError::text(format!("self-issue decode error: {e}")))?;
    CACHED_KEY.with(|c| *c.borrow_mut() = Some(issued.api_key.clone()));
    if let Some(storage) = local_storage() {
        let _ = storage.set_item(STORAGE_KEY, &issued.api_key);
    }
    Ok(issued.api_key)
}

/// 認証付き単項呼び出し。キーが失効していた(UNAUTHENTICATED)ら自己発行し直して1回だけ再試行する。
/// 要求・応答は型付きの`Debug`表示でも記録する(RPC Inspector用)。
async fn rpc<Req: Message + std::fmt::Debug, Resp: Message + Default + std::fmt::Debug>(
    method: &str,
    req: &Req,
) -> Result<Resp, String> {
    let key = get_or_issue_api_key().await.map_err(RpcError::into_string)?;
    let bytes = match call_once(method, req, Some(&key)).await {
        Ok(b) => b,
        Err(e) if e.code == code::UNAUTHENTICATED => {
            clear_cached_api_key();
            let fresh = get_or_issue_api_key().await.map_err(RpcError::into_string)?;
            call_once(method, req, Some(&fresh)).await.map_err(RpcError::into_string)?
        },
        Err(e) => return Err(e.into_string()),
    };
    let resp = Resp::decode(&bytes[..]).map_err(|e| format!("decode error: {e}"))?;
    // 直近の記録に、型付きの中身を足す。
    LOG.with(|l| {
        if let Some(last) = l.borrow_mut().iter_mut().rev().find(|e| e.method == method) {
            last.request = format!("{req:?}");
            last.response = format!("{resp:?}");
        }
    });
    Ok(resp)
}

/// `google.protobuf.Struct`で返るRPCを、REST時代と同じJSON形の型へデコードする。
async fn rpc_json<Req: Message + std::fmt::Debug, T: DeserializeOwned>(method: &str, req: &Req) -> Result<T, String> {
    let s: open_runo_proto::prost_types::Struct = rpc(method, req).await?;
    serde_json::from_value(struct_to_json(&s)).map_err(|e| format!("decode error: {e}"))
}

#[derive(Debug, Deserialize)]
pub struct Health {
    pub status: String,
    pub service: String,
    pub version: String,
}

pub async fn health_check() -> Result<Health, String> {
    let f = fetch_bytes("GET", "/health", None, None, None, &[])
        .await
        .map_err(RpcError::into_string)?;
    if !f.ok {
        return Err(format!("HTTP {}", f.status));
    }
    serde_json::from_slice(&f.bytes).map_err(|e| format!("decode error: {e}"))
}

fn schema_version(v: v1::SchemaVersion) -> SchemaVersion {
    SchemaVersion {
        id: v.id,
        namespace: v.namespace,
        service_name: v.service_name,
        sdl: v.sdl,
        stage: v.stage,
        created_at: v.created_at,
    }
}

pub async fn register_schema(service_name: &str, sdl: &str, stage: &str) -> Result<SchemaVersion, String> {
    let v: v1::SchemaVersion = rpc(
        "RegisterSchema",
        &v1::RegisterSchemaRequest {
            service_name: service_name.to_string(),
            sdl: sdl.to_string(),
            stage: Some(stage.to_string()),
            namespace: None,
        },
    )
    .await?;
    Ok(schema_version(v))
}

/// SDLファイルを読み、本文を`RegisterSchema`に渡す(旧multipartアップロードの置き換え)。
pub async fn register_schema_upload(
    service_name: &str,
    stage: &str,
    file: &web_sys::File,
) -> Result<SchemaVersion, String> {
    let sdl = JsFuture::from(file.text())
        .await
        .map_err(|e| format!("file read error: {e:?}"))?
        .as_string()
        .ok_or("file is not text")?;
    register_schema(service_name, &sdl, stage).await
}

pub async fn get_schema_history(service: &str) -> Result<SchemaHistoryResponse, String> {
    let r: v1::SchemaHistoryResponse = rpc(
        "SchemaHistory",
        &v1::SchemaHistoryRequest {
            service_name: service.to_string(),
            namespace: None,
        },
    )
    .await?;
    Ok(SchemaHistoryResponse {
        versions: r.versions.into_iter().map(schema_version).collect(),
    })
}

pub async fn federation_status() -> Result<FederationStatusResponse, String> {
    let r: v1::FederationStatusResponse = rpc("FederationStatus", &v1::Empty {}).await?;
    Ok(FederationStatusResponse {
        contributing_services: r.contributing_services,
        type_count: r.type_count as usize,
        field_count: r.field_count as usize,
    })
}

#[derive(Debug, Serialize)]
pub struct AiRouteCandidate<'a> {
    pub provider: &'a str,
    pub estimated_cost_usd_per_1k_tokens: f64,
    pub estimated_latency_ms: u32,
    pub is_local: bool,
    pub context_length: u32,
}

#[derive(Debug, Deserialize)]
pub struct AiRouteResponse {
    pub selected_provider: String,
    pub is_local: bool,
    pub estimated_cost_usd_per_1k_tokens: f64,
    pub estimated_latency_ms: u32,
}

pub async fn ai_route(
    policy: &str,
    candidates: Vec<AiRouteCandidate<'_>>,
) -> Result<AiRouteResponse, String> {
    rpc_json(
        "AiRoute",
        &v1::AiRouteRequest {
            policy: policy.to_string(),
            min_context_length: None,
            candidates: candidates
                .iter()
                .map(|c| v1::RouteCandidate {
                    provider: c.provider.to_string(),
                    estimated_cost_usd_per_1k_tokens: c.estimated_cost_usd_per_1k_tokens,
                    estimated_latency_ms: c.estimated_latency_ms,
                    is_local: c.is_local,
                    context_length: c.context_length,
                })
                .collect(),
        },
    )
    .await
}

pub async fn db_list(table: &str) -> Result<DbRecordListResponse, String> {
    rpc_json("DbList", &v1::DbTable { table: table.to_string() }).await
}

pub async fn db_get(table: &str, key: &str) -> Result<DbRecordResponse, String> {
    rpc_json("DbGet", &v1::DbKey { table: table.to_string(), key: key.to_string() }).await
}

pub async fn db_put(table: &str, key: &str, value_json: &str) -> Result<DbRecordResponse, String> {
    let value: Value =
        serde_json::from_str(value_json).map_err(|e| format!("invalid JSON value: {e}"))?;
    rpc_json(
        "DbPut",
        &v1::DbPutRequest {
            table: table.to_string(),
            key: key.to_string(),
            value: Some(open_runo_proto::json_to_value(&value)),
        },
    )
    .await
}

pub async fn db_delete(table: &str, key: &str) -> Result<(), String> {
    let _: Value = rpc_json("DbDelete", &v1::DbKey { table: table.to_string(), key: key.to_string() }).await?;
    Ok(())
}

fn flag(f: v1::FeatureFlag) -> FeatureFlagResponse {
    FeatureFlagResponse {
        name: f.name,
        enabled: f.enabled,
        rollout_percent: u8::try_from(f.rollout_percent).unwrap_or(u8::MAX),
        description: f.description,
    }
}

pub async fn feature_flag_list() -> Result<FeatureFlagListResponse, String> {
    let r: v1::ListFeatureFlagsResponse = rpc("ListFeatureFlags", &v1::Empty {}).await?;
    Ok(FeatureFlagListResponse {
        flags: r.flags.into_iter().map(flag).collect(),
    })
}

pub async fn feature_flag_upsert(
    name: &str,
    enabled: bool,
    rollout_percent: u8,
    description: &str,
) -> Result<FeatureFlagResponse, String> {
    let f: v1::FeatureFlag = rpc(
        "UpsertFeatureFlag",
        &v1::UpsertFeatureFlagRequest {
            name: name.to_string(),
            enabled: Some(enabled),
            rollout_percent: Some(u32::from(rollout_percent)),
            description: Some(description.to_string()),
        },
    )
    .await?;
    Ok(flag(f))
}

pub async fn feature_flag_evaluate(name: &str, bucket_key: &str) -> Result<FeatureFlagEvaluationResponse, String> {
    let e: v1::FlagEvaluation = rpc(
        "EvaluateFeatureFlag",
        &v1::EvaluateFlagRequest {
            name: name.to_string(),
            bucket_key: bucket_key.to_string(),
        },
    )
    .await?;
    Ok(FeatureFlagEvaluationResponse {
        name: e.name,
        bucket_key: e.bucket_key,
        enabled: e.enabled,
    })
}

pub async fn feature_flag_delete(name: &str) -> Result<(), String> {
    let _: v1::Empty = rpc("DeleteFeatureFlag", &v1::FlagName { name: name.to_string() }).await?;
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct ScimUser {
    pub id: String,
    #[serde(rename = "userName")]
    pub user_name: String,
    pub active: bool,
    pub roles: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct ScimUserList {
    #[serde(rename = "totalResults")]
    pub total_results: usize,
    #[serde(rename = "Resources")]
    pub resources: Vec<ScimUser>,
}

pub async fn scim_list_users() -> Result<ScimUserList, String> {
    rpc_json("ScimListUsers", &v1::ScimListRequest { filter: None }).await
}

pub async fn scim_create_user(user_name: &str, roles: Vec<&str>) -> Result<Value, String> {
    rpc_json(
        "ScimCreateUser",
        &v1::ScimCreateUserRequest {
            user_name: user_name.to_string(),
            roles: roles.into_iter().map(str::to_string).collect(),
        },
    )
    .await
}

pub async fn scim_delete_user(id: &str) -> Result<(), String> {
    let _: v1::Empty = rpc("ScimDeleteUser", &v1::ScimUserId { id: id.to_string() }).await?;
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct PqRegisterResponse {
    pub hash: String,
    pub registered_at: String,
}

#[derive(Debug, Deserialize)]
pub struct PqQueryResponse {
    pub hash: String,
    pub query: String,
    pub registered_at: String,
}

pub async fn register_persisted_query(query: &str) -> Result<PqRegisterResponse, String> {
    let r: v1::PersistedQuery = rpc(
        "RegisterPersistedQuery",
        &v1::RegisterPersistedQueryRequest { query: query.to_string() },
    )
    .await?;
    Ok(PqRegisterResponse {
        hash: r.hash,
        registered_at: r.registered_at,
    })
}

pub async fn get_persisted_query(hash: &str) -> Result<PqQueryResponse, String> {
    let r: v1::PersistedQuery = rpc("GetPersistedQuery", &v1::PersistedQueryHash { hash: hash.to_string() }).await?;
    Ok(PqQueryResponse {
        hash: r.hash,
        query: r.query,
        registered_at: r.registered_at,
    })
}

#[derive(Debug, Deserialize)]
pub struct PurgeResponse {
    pub purged: String,
}

#[derive(Debug, Deserialize)]
pub struct AiStatsResponse {
    pub ai_enabled: bool,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub hit_ratio: f64,
    pub tracked_keys: usize,
}

pub async fn cache_purge(path: &str) -> Result<PurgeResponse, String> {
    let r: v1::PurgeResponse = rpc("PurgeCache", &v1::PurgeRequest { path: path.to_string() }).await?;
    Ok(PurgeResponse { purged: r.purged })
}

pub async fn cache_purge_all() -> Result<PurgeResponse, String> {
    let r: v1::PurgeResponse = rpc("PurgeAllCache", &v1::Empty {}).await?;
    Ok(PurgeResponse { purged: r.purged })
}

pub async fn cache_ai_stats() -> Result<AiStatsResponse, String> {
    rpc_json("CacheAiStats", &v1::Empty {}).await
}

#[derive(Debug, Deserialize)]
pub struct ExportResponse {
    pub written: Vec<String>,
    pub records: usize,
}

#[derive(Debug, Deserialize)]
pub struct IntegrityResponse {
    pub backend: String,
    pub healed: usize,
}

pub async fn backup_export() -> Result<ExportResponse, String> {
    rpc_json("BackupExport", &v1::Empty {}).await
}

pub async fn integrity_check() -> Result<IntegrityResponse, String> {
    rpc_json("IntegrityCheck", &v1::Empty {}).await
}

// ── Analytics (docs/cosmo-parity.md 4a) ─────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct MonthlyCount {
    pub month: String,
    pub count: u64,
}

#[derive(Debug, Deserialize)]
pub struct RequestsPerMonthResponse {
    pub months: Vec<MonthlyCount>,
}

#[derive(Debug, Deserialize)]
pub struct OperationSummary {
    pub method: String,
    pub path: String,
    pub count: u64,
    pub error_count: u64,
    pub avg_duration_ms: f64,
    pub error_rate: f64,
}

#[derive(Debug, Deserialize)]
pub struct OperationsResponse {
    pub operations: Vec<OperationSummary>,
}

pub async fn requests_per_month() -> Result<RequestsPerMonthResponse, String> {
    rpc_json("AnalyticsRequestsPerMonth", &v1::Empty {}).await
}

pub async fn operations_summary() -> Result<OperationsResponse, String> {
    rpc_json("AnalyticsOperations", &v1::Empty {}).await
}

// ── サーバーストリーミング(Connectのエンベロープを ReadableStream で読む) ──────────

/// 購読の停止用ハンドル。
pub struct WatchHandle {
    abort: web_sys::AbortController,
}

impl WatchHandle {
    /// 購読を止める(`fetch`を中断する)。
    pub fn stop(&self) {
        self.abort.abort();
    }
}

/// `WatchEvents`(スキーマ登録・履歴の増加)を購読する。イベントごとに`on_event`、終了・失敗時に
/// `on_end`(正常終了なら空文字)が呼ばれる。複数の購読・単項呼び出しは同時に走らせられる。
pub fn watch_events(
    on_event: impl Fn(v1::AdminEvent) + 'static,
    on_end: impl Fn(String) + 'static,
) -> Result<WatchHandle, String> {
    let abort = web_sys::AbortController::new().map_err(|e| format!("{e:?}"))?;
    let signal = abort.signal();
    wasm_bindgen_futures::spawn_local(async move {
        let result = run_watch(&signal, &on_event).await;
        on_end(result.err().unwrap_or_default());
    });
    Ok(WatchHandle { abort })
}

async fn run_watch(signal: &web_sys::AbortSignal, on_event: &dyn Fn(v1::AdminEvent)) -> Result<(), String> {
    let key = get_or_issue_api_key().await.map_err(RpcError::into_string)?;
    let js = |e: wasm_bindgen::JsValue| format!("{e:?}");
    let opts = RequestInit::new();
    opts.set_method("POST");
    opts.set_mode(RequestMode::SameOrigin);
    opts.set_signal(Some(signal));
    opts.set_body(&js_sys::Uint8Array::from(encode_frame(&v1::Empty {}.encode_to_vec()).as_slice()));
    let request = Request::new_with_str_and_init(&open_runo_proto::method_path("WatchEvents"), &opts).map_err(js)?;
    let h = request.headers();
    h.set("content-type", "application/connect+proto").map_err(js)?;
    h.set("connect-protocol-version", "1").map_err(js)?;
    h.set("x-api-key", &key).map_err(js)?;
    let window = web_sys::window().ok_or("no window")?;
    let resp: Response = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(js)?
        .dyn_into()
        .map_err(js)?;
    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let body = resp.body().ok_or("response has no body stream")?;
    let reader: web_sys::ReadableStreamDefaultReader = body
        .get_reader()
        .dyn_into()
        .map_err(|e: js_sys::Object| format!("{e:?}"))?;
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let chunk = match JsFuture::from(reader.read()).await {
            Ok(c) => c,
            // 中断(AbortController)は正常な停止として扱う。
            Err(_) if signal.aborted() => return Ok(()),
            Err(e) => return Err(js(e)),
        };
        let done = js_sys::Reflect::get(&chunk, &"done".into()).ok().and_then(|d| d.as_bool()).unwrap_or(true);
        if done {
            return Ok(());
        }
        let value = js_sys::Reflect::get(&chunk, &"value".into()).map_err(js)?;
        buf.extend_from_slice(&js_sys::Uint8Array::new(&value).to_vec());
        while let Some((flag, msg, tail)) = split_frame(&buf.clone()) {
            let consumed = buf.len() - tail.len();
            if flag == 0 {
                on_event(v1::AdminEvent::decode(msg).map_err(|e| format!("decode error: {e}"))?);
            } else if flag & 0x02 != 0 {
                // 終端エンベロープ: `{}`なら正常、`{"error":{...}}`なら失敗。
                let end: Value = serde_json::from_slice(msg).unwrap_or(Value::Null);
                return match end["error"]["message"].as_str() {
                    Some(m) => Err(m.to_string()),
                    None => Ok(()),
                };
            }
            buf.drain(..consumed);
        }
    }
}

/// 同時実行の確認用: 単項呼び出しを`n`個**並列**に走らせ、`(成功数, 全体の所要ms)`を返す。
pub async fn concurrency_probe(n: usize) -> (usize, f64) {
    let started = js_sys::Date::now();
    let calls = (0..n).map(|_| federation_status());
    let results = futures::future::join_all(calls).await;
    (results.iter().filter(|r| r.is_ok()).count(), js_sys::Date::now() - started)
}
