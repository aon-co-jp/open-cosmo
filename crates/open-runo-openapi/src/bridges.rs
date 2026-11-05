//! 同じ操作定義から、Cosmo(GraphQL)・Tauri(invoke)・TypeScript向けの
//! 出力を作る。

use crate::{Method, Operation, Spec, StreamKind};
use serde_json::{json, Value};
use std::collections::BTreeSet;

/// TypeScriptクライアントの呼び出し方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TsClient {
    /// `fetch` でHTTPを呼ぶ(ブラウザ/Node)。
    Fetch,
    /// Tauriの `invoke` でコマンドを呼ぶ。
    TauriInvoke,
}

fn ident(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.chars().next().is_none_or(|c| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

fn snake(s: &str) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else if c.is_ascii_alphanumeric() {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    out
}

fn ref_name(schema: &Value) -> Option<&str> {
    schema
        .get("$ref")?
        .as_str()?
        .strip_prefix("#/components/schemas/")
}

fn first_success_schema(op: &Operation) -> Option<&Value> {
    op.responses
        .iter()
        .find(|(k, _)| k.starts_with('2'))
        .and_then(|(_, v)| {
            // JSONを優先し、無ければ最初のコンテンツ種別(text/plain等)のスキーマ。
            v.pointer("/content/application~1json/schema").or_else(|| {
                v.get("content")
                    .and_then(Value::as_object)
                    .and_then(|c| c.values().next())
                    .and_then(|m| m.get("schema"))
            })
        })
}

fn is_multipart(op: &Operation) -> bool {
    op.request_body
        .as_ref()
        .and_then(|b| b.pointer("/content/multipart~1form-data"))
        .is_some()
}

/// SSE/WebSocketの操作で、サーバーから流れてくる1メッセージのスキーマ。
fn stream_event_schema(op: &Operation) -> Option<&Value> {
    match op.stream? {
        StreamKind::Sse => first_success_schema(op),
        StreamKind::WebSocket => op.extensions.get("x-websocket")?.get("server"),
    }
}

/// WebSocketでクライアントが送る1メッセージのスキーマ。
fn ws_client_schema(op: &Operation) -> Option<&Value> {
    op.extensions.get("x-websocket")?.get("client")
}

fn body_schema(op: &Operation) -> Option<&Value> {
    op.request_body
        .as_ref()?
        .pointer("/content/application~1json/schema")
}

fn is_nullable(s: &Value) -> bool {
    s.get("nullable").and_then(Value::as_bool) == Some(true)
}

// ---------------------------------------------------------------- GraphQL

fn gql_type(s: &Value, input: bool) -> String {
    if let Some(n) = ref_name(s) {
        return if input {
            format!("{}Input", ident(n))
        } else {
            ident(n)
        };
    }
    if let Some(Value::Array(v)) = s.get("allOf") {
        if let Some(n) = v.first().and_then(ref_name) {
            return if input {
                format!("{}Input", ident(n))
            } else {
                ident(n)
            };
        }
    }
    match s.get("type").and_then(Value::as_str) {
        Some("string") => "String".into(),
        Some("integer") => "Int".into(),
        Some("number") => "Float".into(),
        Some("boolean") => "Boolean".into(),
        Some("array") => {
            let inner = s
                .get("items")
                .map_or("JSON".into(), |i| gql_field_type(i, true, input));
            format!("[{inner}]")
        },
        _ => "JSON".into(),
    }
}

fn gql_field_type(s: &Value, required: bool, input: bool) -> String {
    let t = gql_type(s, input);
    if required && !is_nullable(s) {
        format!("{t}!")
    } else {
        t
    }
}

fn gql_objects(spec_doc: &Value, input_names: &BTreeSet<String>, out: &mut String) {
    let Some(schemas) = spec_doc
        .pointer("/components/schemas")
        .and_then(Value::as_object)
    else {
        return;
    };
    for (name, s) in schemas {
        let Some(props) = s.get("properties").and_then(Value::as_object) else {
            if let Some(Value::Array(vals)) = s.get("enum") {
                let names: Vec<String> =
                    vals.iter().filter_map(|v| v.as_str().map(ident)).collect();
                if !names.is_empty() {
                    out.push_str(&format!(
                        "enum {} {{ {} }}\n\n",
                        ident(name),
                        names.join(" ")
                    ));
                }
            }
            continue;
        };
        let required: BTreeSet<&str> = s
            .get("required")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        for input in [false, true] {
            if input && !input_names.contains(name) {
                continue;
            }
            let kw = if input { "input" } else { "type" };
            let suffix = if input { "Input" } else { "" };
            out.push_str(&format!("{kw} {}{suffix} {{\n", ident(name)));
            for (f, fs) in props {
                out.push_str(&format!(
                    "  {}: {}\n",
                    ident(f),
                    gql_field_type(fs, required.contains(f.as_str()), input)
                ));
            }
            if props.is_empty() {
                out.push_str("  _empty: Boolean\n");
            }
            out.push_str("}\n\n");
        }
    }
}

impl Spec {
    /// Cosmo(WunderGraph)互換のGraphQL SDL。GETは `Query`、それ以外は
    /// `Mutation` のフィールドになり、フィールド名は operationId。
    /// 型は `components.schemas` から生成し、構造を持たない値は `JSON`
    /// スカラーにする。Cosmoのサブグラフとしてそのまま登録できる。
    pub fn to_graphql_sdl(&self) -> String {
        let doc = self.build();
        let mut input_names = BTreeSet::new();
        for op in self.operations() {
            if let Some(n) = body_schema(op).and_then(ref_name) {
                input_names.insert(n.to_owned());
            }
        }
        let mut queries = String::new();
        let mut mutations = String::new();
        let mut subscriptions = String::new();
        for op in self.operations() {
            let mut args: Vec<String> = op
                .path_params
                .iter()
                .map(|p| format!("{}: String!", ident(p)))
                .collect();
            for p in &op.params {
                let (Some(name), Some(sch)) =
                    (p.get("name").and_then(Value::as_str), p.get("schema"))
                else {
                    continue;
                };
                let req = p.get("required").and_then(Value::as_bool) == Some(true);
                args.push(format!(
                    "{}: {}",
                    ident(name),
                    gql_field_type(sch, req, true)
                ));
            }
            if let Some(b) = body_schema(op) {
                args.push(format!("input: {}", gql_field_type(b, true, true)));
            } else if op.request_body.is_some() {
                args.push("input: String!".into());
            }
            let ret_schema = if op.stream.is_some() {
                stream_event_schema(op)
            } else {
                first_success_schema(op)
            };
            let ret = ret_schema.map_or("JSON".to_owned(), |s| gql_field_type(s, false, false));
            let arglist = if args.is_empty() {
                String::new()
            } else {
                format!("({})", args.join(", "))
            };
            let line = format!(
                "  \"\"\"{} {}\"\"\"\n  {}{}: {}\n",
                op.method.key().to_uppercase(),
                op.path,
                ident(&op.operation_id()),
                arglist,
                ret
            );
            if op.stream.is_some() {
                subscriptions.push_str(&line);
            } else if op.method == Method::Get {
                queries.push_str(&line);
            } else {
                mutations.push_str(&line);
            }
        }
        let mut out = String::from("scalar JSON\n\n");
        gql_objects(&doc, &input_names, &mut out);
        if queries.is_empty() {
            // Queryが空のスキーマは無効なので、最低限のフィールドを置く。
            queries.push_str("  _service: String\n");
        }
        out.push_str(&format!("type Query {{\n{queries}}}\n"));
        if !mutations.is_empty() {
            out.push_str(&format!("\ntype Mutation {{\n{mutations}}}\n"));
        }
        if !subscriptions.is_empty() {
            out.push_str(&format!("\ntype Subscription {{\n{subscriptions}}}\n"));
        }
        out
    }

    // ------------------------------------------------------------- Tauri

    /// Tauri互換のinvokeマニフェスト。`invoke(name, args)` で呼べる
    /// コマンド名(snake_case)と引数・戻り値のスキーマを返す。
    pub fn to_tauri_manifest(&self) -> Value {
        let doc = self.build();
        let commands: Vec<Value> = self
            .operations()
            .iter()
            .map(|op| {
                let query: Vec<Value> = op.params.iter().filter_map(|p| p.get("name").cloned()).collect();
                json!({
                    "name": snake(&op.operation_id()),
                    "summary": op.summary,
                    "http": { "method": op.method.key(), "path": op.path },
                    "args": {
                        "path": op.path_params,
                        "query": query,
                        "body": body_schema(op).cloned(),
                    },
                    "returns": first_success_schema(op).cloned(),
                    "stream": op.stream.map(|k| match k { StreamKind::Sse => "sse", StreamKind::WebSocket => "websocket" }),
                })
            })
            .collect();
        json!({
            "info": doc["info"],
            "commands": commands,
            "schemas": doc["components"]["schemas"],
        })
    }

    // -------------------------------------------------------- TypeScript

    /// TypeScriptの型定義とクライアント。`Fetch` はHTTP、`TauriInvoke` は
    /// Tauriの `invoke` を呼ぶ。どちらも同じ引数オブジェクト
    /// (`{ path params, query params, body }`)を取る。
    pub fn to_typescript(&self, client: TsClient) -> String {
        let doc = self.build();
        let mut out = String::from("// 自動生成(open-runo-openapi)。手で編集しない。\n\n");
        if let Some(schemas) = doc
            .pointer("/components/schemas")
            .and_then(Value::as_object)
        {
            for (name, s) in schemas {
                out.push_str(&format!(
                    "export type {} = {};\n\n",
                    ident(name),
                    ts_type(s)
                ));
            }
        }
        match client {
            TsClient::Fetch => {
                out.push_str(concat!(
                    "export function createClient(baseUrl: string, headers: Record<string, string> = {}) {\n",
                    "  function url(path: string, query: Record<string, unknown>) {\n",
                    "    const qs = new URLSearchParams();\n",
                    "    for (const [k, v] of Object.entries(query)) {\n",
                    "      if (v === undefined || v === null) continue;\n",
                    "      if (Array.isArray(v)) for (const x of v) qs.append(k, String(x)); else qs.set(k, String(v));\n",
                    "    }\n",
                    "    const q = qs.toString();\n",
                    "    return baseUrl + path + (q ? '?' + q : '');\n",
                    "  }\n",
                    "  async function call(method: string, path: string, query: Record<string, unknown>, body?: unknown) {\n",
                    "    const isForm = typeof FormData !== 'undefined' && body instanceof FormData;\n",
                    "    const res = await fetch(url(path, query), {\n",
                    "      method,\n",
                    "      headers: { ...(body === undefined || isForm ? {} : { 'Content-Type': 'application/json' }), ...headers },\n",
                    "      body: body === undefined ? undefined : isForm ? (body as FormData) : JSON.stringify(body),\n",
                    "    });\n",
                    "    if (!res.ok) throw new Error(`${method} ${path}: ${res.status}`);\n",
                    "    const text = await res.text();\n",
                    "    const json = (res.headers.get('content-type') ?? '').includes('json');\n",
                    "    return text ? (json ? JSON.parse(text) : text) : undefined;\n",
                    "  }\n",
                    "  // SSE/WebSocketはブラウザの仕組み上ヘッダを付けられない(認証はクエリ等で)。\n",
                    "  return {\n"
                ));
            },
            TsClient::TauriInvoke => {
                out.push_str(
                    "import { invoke } from '@tauri-apps/api/core';\n\nexport const api = {\n",
                );
            },
        }
        for op in self.operations() {
            let id = ident(&op.operation_id());
            let mut fields: Vec<String> = op
                .path_params
                .iter()
                .map(|p| format!("{}: string", ident(p)))
                .collect();
            for p in &op.params {
                if let (Some(n), Some(s)) = (p.get("name").and_then(Value::as_str), p.get("schema"))
                {
                    let opt = if p.get("required").and_then(Value::as_bool) == Some(true) {
                        ""
                    } else {
                        "?"
                    };
                    fields.push(format!("{}{opt}: {}", ident(n), ts_type(s)));
                }
            }
            if let Some(b) = body_schema(op) {
                fields.push(format!("body: {}", ts_type(b)));
            } else if is_multipart(op) {
                fields.push("body: FormData".into());
            } else if op.request_body.is_some() {
                fields.push("body: string | Uint8Array".into());
            }
            let ret = first_success_schema(op).map_or("unknown".to_owned(), ts_type);
            let args = if fields.is_empty() {
                String::new()
            } else {
                format!("args: {{ {} }}", fields.join("; "))
            };
            if let Some(kind) = op.stream {
                // 長く続く接続は、Promiseではなくコールバックで受ける(Tauriのinvokeでは扱えない)。
                if client == TsClient::Fetch {
                    let mut path = format!("`{}`", op.path);
                    for p in &op.path_params {
                        path = path.replace(
                            &format!("{{{p}}}"),
                            &format!("${{encodeURIComponent(args.{})}}", ident(p)),
                        );
                    }
                    let qkeys: Vec<String> = op
                        .params
                        .iter()
                        .filter_map(|p| p.get("name").and_then(Value::as_str))
                        .map(|n| {
                            format!(
                                "{}: args.{}",
                                serde_json::to_string(n).unwrap_or_default(),
                                ident(n)
                            )
                        })
                        .collect();
                    let summary = op.summary.clone().unwrap_or_else(|| op.route_label());
                    let ev = stream_event_schema(op).map_or("unknown".to_owned(), ts_type);
                    let a = if fields.is_empty() {
                        String::new()
                    } else {
                        format!("{args}, ")
                    };
                    match kind {
                        StreamKind::Sse => {
                            let names: Vec<String> = op
                                .extensions
                                .get("x-sse-events")
                                .and_then(Value::as_array)
                                .map(|a| {
                                    a.iter()
                                        .filter_map(|v| v.as_str().map(str::to_owned))
                                        .collect()
                                })
                                .unwrap_or_default();
                            // イベント名つきは名前ごとに購読し、第1引数でイベント名も渡す。
                            let (cb, listen) = if names.is_empty() {
                                (
                                    format!("onEvent: (e: {ev}) => void"),
                                    "      es.onmessage = (m) => onEvent(JSON.parse(m.data));\n"
                                        .to_owned(),
                                )
                            } else {
                                let union = names
                                    .iter()
                                    .map(|n| serde_json::to_string(n).unwrap_or_default())
                                    .collect::<Vec<_>>()
                                    .join(" | ");
                                let mut l = String::new();
                                for n in &names {
                                    let q = serde_json::to_string(n).unwrap_or_default();
                                    l.push_str(&format!("      es.addEventListener({q}, (m) => onEvent({q}, JSON.parse((m as MessageEvent).data)));\n"));
                                }
                                (format!("onEvent: (name: {union}, e: {ev}) => void"), l)
                            };
                            out.push_str(&format!(
                                "    /** {summary}(SSE) 戻り値の関数で切断する */\n    {id}: ({a}{cb}, onError?: (e: Event) => void): (() => void) => {{\n      const es = new EventSource(url({path}, {{ {} }}));\n{listen}      if (onError) es.onerror = onError;\n      return () => es.close();\n    }},\n",
                                qkeys.join(", ")
                            ));
                        },
                        StreamKind::WebSocket => {
                            let sent = ws_client_schema(op).map_or("unknown".to_owned(), ts_type);
                            let bin = op.extensions.get("x-websocket-binary");
                            let to_server = bin
                                .and_then(|b| b.get("client"))
                                .is_some_and(|v| !v.is_null());
                            let from_server = bin
                                .and_then(|b| b.get("server"))
                                .is_some_and(|v| !v.is_null());
                            let on_bin = if from_server {
                                ", onBinary?: (data: ArrayBuffer) => void"
                            } else {
                                ""
                            };
                            let send_bin = if to_server {
                                "; sendBinary: (data: ArrayBuffer | Uint8Array) => void"
                            } else {
                                ""
                            };
                            let recv = if from_server {
                                "      ws.binaryType = 'arraybuffer';\n      ws.onmessage = (m) => (typeof m.data === 'string' ? onMessage(JSON.parse(m.data)) : onBinary?.(m.data as ArrayBuffer));\n"
                            } else {
                                "      ws.onmessage = (m) => onMessage(JSON.parse(m.data));\n"
                            };
                            let send_impl = if to_server {
                                ", sendBinary: (d) => ws.send(d)"
                            } else {
                                ""
                            };
                            out.push_str(&format!(
                                "    /** {summary}(WebSocket) */\n    {id}: ({a}onMessage: (m: {ev}) => void, onClose?: () => void{on_bin}): {{ send: (m: {sent}) => void; close: () => void{send_bin} }} => {{\n      const ws = new WebSocket(url({path}, {{ {} }}).replace(/^http/, 'ws'));\n{recv}      if (onClose) ws.onclose = onClose;\n      return {{ send: (m) => ws.send(JSON.stringify(m)), close: () => ws.close(){send_impl} }};\n    }},\n",
                                qkeys.join(", ")
                            ));
                        },
                    }
                }
                continue;
            }
            match client {
                TsClient::TauriInvoke => {
                    let a = if fields.is_empty() { "" } else { ", args" };
                    out.push_str(&format!(
                        "  /** {} */\n  {id}: ({args}): Promise<{ret}> => invoke<{ret}>('{}'{a}),\n",
                        op.summary.clone().unwrap_or_else(|| op.route_label()),
                        snake(&op.operation_id())
                    ));
                },
                TsClient::Fetch => {
                    let mut path = format!("`{}`", op.path);
                    for p in &op.path_params {
                        path = path.replace(
                            &format!("{{{p}}}"),
                            &format!("${{encodeURIComponent(args.{})}}", ident(p)),
                        );
                    }
                    let qkeys: Vec<String> = op
                        .params
                        .iter()
                        .filter_map(|p| p.get("name").and_then(Value::as_str))
                        .map(|n| {
                            format!(
                                "{}: args.{}",
                                serde_json::to_string(n).unwrap_or_default(),
                                ident(n)
                            )
                        })
                        .collect();
                    let body = if op.request_body.is_some() {
                        ", args.body"
                    } else {
                        ""
                    };
                    out.push_str(&format!(
                        "    /** {} */\n    {id}: ({args}): Promise<{ret}> => call('{}', {path}, {{ {} }}{body}),\n",
                        op.summary.clone().unwrap_or_else(|| op.route_label()),
                        op.method.key().to_uppercase(),
                        qkeys.join(", ")
                    ));
                },
            }
        }
        out.push_str(match client {
            TsClient::Fetch => "  };\n}\n",
            TsClient::TauriInvoke => "};\n",
        });
        out
    }
}

fn ts_type(s: &Value) -> String {
    if let Some(n) = ref_name(s) {
        return ident(n);
    }
    if let Some(Value::Array(v)) = s.get("allOf") {
        if let Some(n) = v.first().and_then(ref_name) {
            return wrap_nullable(s, ident(n));
        }
    }
    if let Some(Value::Array(vals)) = s.get("enum") {
        let lits: Vec<String> = vals.iter().map(|v| v.to_string()).collect();
        if !lits.is_empty() {
            return wrap_nullable(s, lits.join(" | "));
        }
    }
    for key in ["oneOf", "anyOf"] {
        if let Some(Value::Array(v)) = s.get(key) {
            let parts: Vec<String> = v.iter().map(ts_type).collect();
            return wrap_nullable(s, parts.join(" | "));
        }
    }
    let base = match s.get("type").and_then(Value::as_str) {
        Some("string") => "string".into(),
        Some("integer") | Some("number") => "number".to_string(),
        Some("boolean") => "boolean".into(),
        Some("array") => format!(
            "Array<{}>",
            s.get("items").map_or("unknown".into(), ts_type)
        ),
        Some("object") => match s.get("properties").and_then(Value::as_object) {
            Some(props) if !props.is_empty() => {
                let required: BTreeSet<&str> = s
                    .get("required")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                let fields: Vec<String> = props
                    .iter()
                    .map(|(k, v)| {
                        let opt = if required.contains(k.as_str()) {
                            ""
                        } else {
                            "?"
                        };
                        format!(
                            "{}{opt}: {}",
                            serde_json::to_string(k).unwrap_or_default(),
                            ts_type(v)
                        )
                    })
                    .collect();
                format!("{{ {} }}", fields.join("; "))
            },
            _ => match s.get("additionalProperties") {
                Some(a) if a.is_object() => format!("Record<string, {}>", ts_type(a)),
                _ => "Record<string, unknown>".into(),
            },
        },
        _ => "unknown".into(),
    };
    wrap_nullable(s, base)
}

fn wrap_nullable(s: &Value, t: String) -> String {
    if is_nullable(s) {
        format!("{t} | null")
    } else {
        t
    }
}
