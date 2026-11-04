//! Connect(単純呼び出し)の入口。REST 全廃方針(2026-11-04、`API-POLICY.md`)。
//!
//! `POST /open_cosmo.admin.v1.Admin/<Method>`、`content-type: application/json`、
//! 本文は JSON。管理系の操作(スキーマ登録・DB・キャッシュ・バックアップ・
//! フィーチャーフラグ・分析など)はすべてここ経由で呼ぶ。旧 `/api/...` の REST
//! 経路は公開ルーターから外してあり、404 になる。
//!
//! 既存のハンドラー(`handlers_hyper`)は作り直さず、このアダプターが
//! 「JSON 本文のフィールド → パスパラメーター/クエリ/本文」へ振り分けて呼ぶ。
//! 本文の読み込みは RS-JSON の `parse_secure`(サイズ・深さ・重複キーの上限つき)。
//! エラーは Connect 形式 `{"code","message"}` に統一する。
//!
//! 移さないもの(相手の規格が形を決める/ストリーミング)は `lib.rs` の
//! 「例外」コメントと `CLAUDE.md` の台帳を参照。

use crate::hyper_compat::{fixed_body, json_response, Handler, Params, PrefetchedBody, Response};
use bytes::Bytes;
use http_body_util::BodyExt;
use hyper::{Method, StatusCode};
use rust_json::secure::{parse_secure, Limits};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::Arc;

/// Connect のサービス名(`/<SERVICE>/<Method>`)。
pub const SERVICE: &str = "open_cosmo.admin.v1.Admin";

/// 本文の上限(2 MiB)。スキーマ SDL を含められる大きさ。
const MAX_BODY: usize = 2 * 1024 * 1024;

/// 既存ハンドラーへ渡す入力の作り方。
#[derive(Clone, Copy)]
pub enum Input {
    /// 残りのフィールドをクエリ文字列にする(元が GET の操作)。
    Query,
    /// 残りのフィールドを JSON 本文として渡す(元が POST/PUT の操作)。
    Body,
    /// 指定フィールドの文字列を生の本文にし、残りをクエリにする(SDL の直接投入)。
    RawText(&'static str),
}

/// Connect の 1 メソッドと、それを処理する既存ハンドラー。
pub struct Op {
    pub name: &'static str,
    pub method: Method,
    /// パスパラメーター名(JSON のフィールドから取り出す)。
    pub params: &'static [&'static str],
    pub input: Input,
    pub handler: Handler,
}

pub fn op(name: &'static str, method: Method, params: &'static [&'static str], input: Input, handler: Handler) -> Op {
    Op { name, method, params, input, handler }
}

/// HTTP ステータスを Connect のエラーコードへ。
fn code_for(status: StatusCode) -> &'static str {
    match status.as_u16() {
        400 | 422 => "invalid_argument",
        401 => "unauthenticated",
        403 => "permission_denied",
        404 => "not_found",
        405 => "unimplemented",
        409 => "already_exists",
        413 => "resource_exhausted",
        429 => "resource_exhausted",
        501 => "unimplemented",
        503 => "unavailable",
        s if s >= 500 => "internal",
        _ => "unknown",
    }
}

fn connect_status(status: StatusCode) -> StatusCode {
    // Connect は invalid_argument を 400 にする(422 は使わない)。
    if status == StatusCode::UNPROCESSABLE_ENTITY {
        StatusCode::BAD_REQUEST
    } else {
        status
    }
}

pub fn error_response(status: StatusCode, message: &str) -> Response {
    json_response(
        connect_status(status),
        &serde_json::json!({ "code": code_for(status), "message": message }),
    )
}

fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn scalar_to_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// 内側のハンドラーの応答を Connect 形式へ整える。
async fn normalize(resp: Response) -> Response {
    let (parts, body) = resp.into_parts();
    let bytes = match body.collect().await {
        Ok(c) => c.to_bytes(),
        Err(never) => match never {},
    };
    if parts.status.is_success() {
        let body = if bytes.is_empty() { Bytes::from_static(b"{}") } else { bytes };
        return hyper::Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/json")
            .body(fixed_body(body))
            .expect("fixed headers");
    }
    // 失敗: {"error": "..."} か空本文 → {"code","message"}
    let message = serde_json::from_slice::<Value>(&bytes)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str().map(str::to_string)))
        .unwrap_or_else(|| parts.status.canonical_reason().unwrap_or("error").to_string());
    error_response(parts.status, &message)
}

/// `POST /open_cosmo.admin.v1.Admin/:method` のハンドラー。
pub fn connect_handler(ops: Vec<Op>) -> Handler {
    let table: Arc<HashMap<&'static str, Op>> = Arc::new(ops.into_iter().map(|o| (o.name, o)).collect());
    Arc::new(move |mut req, params: Params| {
        let table = Arc::clone(&table);
        Box::pin(async move {
            let Some(op) = params.get("method").and_then(|m| table.get(m)) else {
                return error_response(StatusCode::NOT_FOUND, "unknown method");
            };
            if let Some(ct) = req.headers().get("content-type").and_then(|v| v.to_str().ok()) {
                if !ct.to_ascii_lowercase().starts_with("application/json") {
                    return error_response(StatusCode::BAD_REQUEST, "content-type must be application/json");
                }
            }
            // Incoming は所有権を奪わずに &mut で読み切る(中身は PrefetchedBody で渡す)。
            let mut buf: Vec<u8> = Vec::new();
            while let Some(frame) = req.body_mut().frame().await {
                match frame {
                    Ok(f) => {
                        if let Some(d) = f.data_ref() {
                            if buf.len() + d.len() > MAX_BODY {
                                return error_response(StatusCode::PAYLOAD_TOO_LARGE, "request body too large");
                            }
                            buf.extend_from_slice(d);
                        }
                    }
                    Err(_) => return error_response(StatusCode::BAD_REQUEST, "failed to read request body"),
                }
            }
            let bytes = Bytes::from(buf);
            let text = match std::str::from_utf8(&bytes) {
                Ok(t) => t,
                Err(_) => return error_response(StatusCode::BAD_REQUEST, "body is not valid UTF-8"),
            };
            let limits = Limits { max_bytes: MAX_BODY, max_depth: 32, reject_duplicate_keys: true };
            let mut fields: Map<String, Value> = if text.trim().is_empty() {
                Map::new()
            } else {
                match parse_secure(text, &limits) {
                    Ok(Value::Object(m)) => m,
                    Ok(_) => return error_response(StatusCode::BAD_REQUEST, "body must be a JSON object"),
                    Err(e) => return error_response(StatusCode::BAD_REQUEST, &format!("invalid JSON body: {e}")),
                }
            };

            // パスパラメーター
            let mut inner_params = HashMap::new();
            for name in op.params {
                match fields.remove(*name).as_ref().and_then(scalar_to_string) {
                    Some(v) if !v.is_empty() => {
                        inner_params.insert((*name).to_string(), v);
                    }
                    _ => return error_response(StatusCode::BAD_REQUEST, &format!("missing field: {name}")),
                }
            }

            // 本文/クエリ
            let mut query = String::new();
            let add_query = |query: &mut String, fields: &Map<String, Value>| {
                for (k, v) in fields {
                    if let Some(s) = scalar_to_string(v) {
                        if !query.is_empty() {
                            query.push('&');
                        }
                        query.push_str(&percent_encode(k));
                        query.push('=');
                        query.push_str(&percent_encode(&s));
                    }
                }
            };
            let inner_body: Bytes = match op.input {
                Input::Query => {
                    add_query(&mut query, &fields);
                    Bytes::new()
                }
                Input::Body => Bytes::from(serde_json::to_vec(&Value::Object(fields)).unwrap_or_default()),
                Input::RawText(field) => {
                    let raw = match fields.remove(field) {
                        Some(Value::String(s)) => s,
                        _ => return error_response(StatusCode::BAD_REQUEST, &format!("missing field: {field}")),
                    };
                    add_query(&mut query, &fields);
                    Bytes::from(raw)
                }
            };

            *req.method_mut() = op.method.clone();
            let uri = if query.is_empty() { "/".to_string() } else { format!("/?{query}") };
            if let Ok(u) = uri.parse() {
                *req.uri_mut() = u;
            }
            req.extensions_mut().insert(PrefetchedBody(inner_body));

            let resp = (op.handler)(req, Params(inner_params)).await;
            normalize(resp).await
        })
    })
}
