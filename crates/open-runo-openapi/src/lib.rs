//! 型からAPIドキュメントを自動生成する(poem-openapi相当、フレームワーク非依存)。
//!
//! 1つの [`Operation`] 定義(メソッド・パス・リクエスト/レスポンス型)から、
//!
//! - OpenAPI 3.0.3 文書([`Spec::build`])
//! - Swagger UI / ReDoc の配信HTML([`docs_html`])
//! - Cosmo(WunderGraph)互換のGraphQL SDL([`Spec::to_graphql_sdl`]、
//!   RESTを使わずGraphQLサブグラフとして取り込める)
//! - Tauri互換のinvokeマニフェスト([`Spec::to_tauri_manifest`])と
//!   TypeScriptクライアント([`Spec::to_typescript`]、`fetch`版/`invoke`版)
//!
//! を出力する。型情報は `schemars::JsonSchema` から取るので、サーバーの
//! 実際の構造体とドキュメントがずれない。パスの `:name` / `{name}` から
//! パスパラメータを自動抽出し、クエリ用の構造体からクエリパラメータを
//! 自動展開する。ルーターには依存しない(どのHTTP層でも、Tauriのコマンド
//! 層でも使える)。

use schemars::generate::SchemaSettings;
use schemars::JsonSchema;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

mod bridges;
pub use bridges::TsClient;

/// `$defs` 形式の参照をOpenAPIの形へ揃える。
fn rewrite_refs(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(s)) = map.get("$ref") {
                if let Some(rest) = s.strip_prefix("#/$defs/") {
                    let r = format!("#/components/schemas/{rest}");
                    map.insert("$ref".into(), Value::String(r));
                }
            }
            map.values_mut().for_each(rewrite_refs);
        },
        Value::Array(a) => a.iter_mut().for_each(rewrite_refs),
        _ => {},
    }
}

fn root_value<T: JsonSchema>() -> Value {
    let root = SchemaSettings::openapi3()
        .into_generator()
        .into_root_schema_for::<T>();
    let mut v = serde_json::to_value(&root).unwrap_or(Value::Null);
    rewrite_refs(&mut v);
    v
}

/// 型 `T` のスキーマを「参照またはインライン本体」と「依存する定義群」に
/// 分けて返す。名前付きの構造体/enumは `components.schemas` へ登録して
/// `$ref` を返し、`Vec<T>` 等はインラインで返す。
fn schema_of<T: JsonSchema>() -> (Value, Vec<(String, Value)>) {
    let mut v = root_value::<T>();
    let Some(obj) = v.as_object_mut() else {
        return (json!({}), vec![]);
    };
    obj.remove("$schema");
    let mut defs: Vec<(String, Value)> = Vec::new();
    if let Some(Value::Object(d)) = obj.remove("$defs") {
        defs.extend(d);
    }
    if let Some(Value::Object(mut c)) = obj.remove("components") {
        if let Some(Value::Object(s)) = c.remove("schemas") {
            defs.extend(s);
        }
    }
    let title = obj.get("title").and_then(Value::as_str).map(str::to_owned);
    let is_named = (obj.get("type").and_then(Value::as_str) == Some("object")
        && obj.contains_key("properties"))
        || obj.contains_key("oneOf")
        || obj.contains_key("anyOf")
        || obj.contains_key("enum");
    match title {
        Some(name) if is_named => {
            defs.push((name.clone(), Value::Object(obj.clone())));
            (
                json!({ "$ref": format!("#/components/schemas/{name}") }),
                defs,
            )
        },
        _ => {
            obj.remove("title");
            (Value::Object(obj.clone()), defs)
        },
    }
}

/// 構造体 `T` の各フィールドをクエリパラメータへ展開する。
fn query_params_of<T: JsonSchema>() -> Vec<Value> {
    let v = root_value::<T>();
    let required: Vec<String> = v
        .get("required")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let mut out = Vec::new();
    if let Some(Value::Object(props)) = v.get("properties") {
        for (name, schema) in props {
            let mut schema = schema.clone();
            let description = schema
                .as_object_mut()
                .and_then(|o| o.remove("description"))
                .and_then(|d| d.as_str().map(str::to_owned));
            let mut p = json!({
                "name": name,
                "in": "query",
                "required": required.contains(name),
                "schema": schema,
            });
            if let Some(d) = description {
                p["description"] = Value::String(d);
            }
            out.push(p);
        }
    }
    out
}

/// HTTPメソッド(小文字がOpenAPIのキーになる)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

impl Method {
    pub fn key(self) -> &'static str {
        match self {
            Method::Get => "get",
            Method::Post => "post",
            Method::Put => "put",
            Method::Patch => "patch",
            Method::Delete => "delete",
        }
    }
}

/// `:id` / `{id}` 混在のパスを `{id}` 形式へ正規化し、パラメータ名を返す。
fn normalize_path(path: &str) -> (String, Vec<String>) {
    let mut names = Vec::new();
    let segs: Vec<String> = path
        .split('/')
        .map(|s| {
            if let Some(n) = s.strip_prefix(':') {
                names.push(n.to_owned());
                format!("{{{n}}}")
            } else if s.len() > 2 && s.starts_with('{') && s.ends_with('}') {
                names.push(s[1..s.len() - 1].to_owned());
                s.to_owned()
            } else {
                s.to_owned()
            }
        })
        .collect();
    (segs.join("/"), names)
}

/// 例: `GET /api/db/{table}/{key}` → `getApiDbByTableByKey`
fn default_operation_id(method: Method, path: &str) -> String {
    let mut id = method.key().to_owned();
    if path.split('/').all(str::is_empty) {
        id.push_str("Root");
    }
    for seg in path.split('/').filter(|s| !s.is_empty()) {
        let is_param = seg.starts_with('{');
        let seg = seg.trim_matches(|c| c == '{' || c == '}');
        if is_param {
            id.push_str("By");
        }
        let mut up = true;
        for c in seg.chars() {
            if c.is_ascii_alphanumeric() {
                id.push(if up { c.to_ascii_uppercase() } else { c });
                up = false;
            } else {
                up = true;
            }
        }
    }
    id
}

/// 1つのAPI操作の定義。
#[derive(Debug, Clone)]
pub struct Operation {
    pub(crate) method: Method,
    pub(crate) path: String,
    pub(crate) path_params: Vec<String>,
    id: Option<String>,
    pub(crate) summary: Option<String>,
    description: Option<String>,
    tags: Vec<String>,
    secured: bool,
    deprecated: bool,
    pub(crate) params: Vec<Value>,
    pub(crate) request_body: Option<Value>,
    pub(crate) responses: BTreeMap<String, Value>,
    schemas: Vec<(String, Value)>,
    /// SSE/WebSocketの操作(通常のリクエスト/レスポンスでは表せない)。
    pub(crate) stream: Option<StreamKind>,
    /// `x-...` の拡張フィールド(WebSocketのメッセージ型など)。
    pub(crate) extensions: BTreeMap<String, Value>,
}

/// 操作が長く続く接続(サーバープッシュ)であることの種別。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamKind {
    /// Server-Sent Events(`text/event-stream`)。
    Sse,
    /// WebSocket(RFC 6455)。
    WebSocket,
}

/// `multipart/form-data` のファイルフィールドの宣言。
#[derive(Debug, Clone)]
pub struct FileField {
    pub name: String,
    pub description: String,
    pub required: bool,
    /// 同じ名前で複数ファイルを受ける(`format: binary` の配列)。
    pub multiple: bool,
}

impl FileField {
    /// 必須のファイルフィールド。
    pub fn required(name: &str, description: &str) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            required: true,
            multiple: false,
        }
    }
    /// 任意のファイルフィールド。
    pub fn optional(name: &str, description: &str) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            required: false,
            multiple: false,
        }
    }
    /// 同名で複数ファイルを受ける。
    pub fn multiple(mut self) -> Self {
        self.multiple = true;
        self
    }
}

impl Operation {
    pub fn new(method: Method, path: &str) -> Self {
        let (path, path_params) = normalize_path(path);
        Self {
            method,
            path,
            path_params,
            id: None,
            summary: None,
            description: None,
            tags: vec![],
            secured: true,
            deprecated: false,
            params: vec![],
            request_body: None,
            responses: BTreeMap::new(),
            schemas: vec![],
            stream: None,
            extensions: BTreeMap::new(),
        }
    }
    pub fn get(path: &str) -> Self {
        Self::new(Method::Get, path)
    }
    pub fn post(path: &str) -> Self {
        Self::new(Method::Post, path)
    }
    pub fn put(path: &str) -> Self {
        Self::new(Method::Put, path)
    }
    pub fn patch(path: &str) -> Self {
        Self::new(Method::Patch, path)
    }
    pub fn delete(path: &str) -> Self {
        Self::new(Method::Delete, path)
    }

    pub fn id(mut self, id: &str) -> Self {
        self.id = Some(id.into());
        self
    }
    pub fn summary(mut self, s: &str) -> Self {
        self.summary = Some(s.into());
        self
    }
    pub fn description(mut self, s: &str) -> Self {
        self.description = Some(s.into());
        self
    }
    pub fn tag(mut self, t: &str) -> Self {
        self.tags.push(t.into());
        self
    }
    /// 認証不要の操作にする(既定は認証必須)。
    pub fn public(mut self) -> Self {
        self.secured = false;
        self
    }
    pub fn deprecated(mut self) -> Self {
        self.deprecated = true;
        self
    }

    /// クエリ用構造体 `T` のフィールドをクエリパラメータへ展開する。
    pub fn query<T: JsonSchema>(mut self) -> Self {
        self.params.extend(query_params_of::<T>());
        self
    }
    /// 型を付けず、文字列のクエリパラメータを1つ足す。
    pub fn query_param(mut self, name: &str, required: bool, description: &str) -> Self {
        self.params.push(json!({
            "name": name, "in": "query", "required": required,
            "description": description, "schema": { "type": "string" }
        }));
        self
    }
    /// JSONリクエストボディ。
    pub fn body<T: JsonSchema>(mut self) -> Self {
        let (s, defs) = schema_of::<T>();
        self.schemas.extend(defs);
        self.request_body = Some(json!({
            "required": true,
            "content": { "application/json": { "schema": s } }
        }));
        self
    }
    /// `multipart/form-data` のボディ。テキストフィールドは構造体 `T` から、
    /// ファイルフィールドは `files` から文書に載る。
    pub fn body_multipart<T: JsonSchema>(mut self, files: &[FileField]) -> Self {
        let (text, defs) = schema_of::<T>();
        self.schemas.extend(defs);
        let mut props = serde_json::Map::new();
        let mut required = Vec::new();
        for f in files {
            let schema = if f.multiple {
                json!({ "type": "array", "items": { "type": "string", "format": "binary" }, "description": f.description })
            } else {
                json!({ "type": "string", "format": "binary", "description": f.description })
            };
            props.insert(f.name.clone(), schema);
            if f.required {
                required.push(Value::String(f.name.clone()));
            }
        }
        let mut file_part = json!({ "type": "object", "properties": props });
        if !required.is_empty() {
            file_part["required"] = Value::Array(required);
        }
        self.request_body = Some(json!({
            "required": true,
            "content": { "multipart/form-data": { "schema": { "allOf": [text, file_part] } } }
        }));
        self
    }
    /// Server-Sent Events。接続すると `T` のJSONがイベントとして流れてくる
    /// (`data:` 行に1イベント1JSON)。
    pub fn response_sse<T: JsonSchema>(mut self, description: &str) -> Self {
        let (s, defs) = schema_of::<T>();
        self.schemas.extend(defs);
        self.responses.insert(
            "200".to_string(),
            json!({ "description": description, "content": { "text/event-stream": { "schema": s } } }),
        );
        self.stream = Some(StreamKind::Sse);
        self
    }
    /// WebSocket。クライアントが `In`、サーバーが `Out` のJSONテキストを送る。
    pub fn websocket<In: JsonSchema, Out: JsonSchema>(mut self, description: &str) -> Self {
        let (cs, cdefs) = schema_of::<In>();
        let (ss, sdefs) = schema_of::<Out>();
        self.schemas.extend(cdefs);
        self.schemas.extend(sdefs);
        self.responses
            .insert("101".to_string(), json!({ "description": description }));
        self.extensions
            .insert("x-websocket".into(), json!({ "client": cs, "server": ss }));
        self.stream = Some(StreamKind::WebSocket);
        self
    }
    /// SSEの取りうるイベント名(`event:` 行)。文書(`x-sse-events`)とTSクライアントに出る。
    pub fn sse_events(mut self, names: &[&str]) -> Self {
        self.extensions.insert("x-sse-events".into(), json!(names));
        self
    }
    /// WebSocketがバイナリメッセージも使うことの宣言(`x-websocket-binary`)。
    /// `client` / `server` はそれぞれの向きのバイナリの意味(使わない向きは `None`)。
    pub fn websocket_binary(mut self, client: Option<&str>, server: Option<&str>) -> Self {
        self.extensions.insert(
            "x-websocket-binary".into(),
            json!({ "client": client, "server": server }),
        );
        self
    }
    /// 型なしのボディ(multipart・生バイト列など)。
    pub fn body_raw(mut self, content_type: &str) -> Self {
        self.request_body = Some(json!({
            "required": true,
            "content": { content_type: { "schema": { "type": "string", "format": "binary" } } }
        }));
        self
    }
    /// 型付きJSONレスポンス。
    pub fn response<T: JsonSchema>(mut self, status: u16, description: &str) -> Self {
        let (s, defs) = schema_of::<T>();
        self.schemas.extend(defs);
        self.responses.insert(
            status.to_string(),
            json!({ "description": description, "content": { "application/json": { "schema": s } } }),
        );
        self
    }
    /// 200の型付きJSONレスポンス(最も多い形の近道)。
    pub fn ok<T: JsonSchema>(self, description: &str) -> Self {
        self.response::<T>(200, description)
    }
    /// ボディ無し(または型なし)のレスポンス。
    pub fn response_empty(mut self, status: u16, description: &str) -> Self {
        self.responses
            .insert(status.to_string(), json!({ "description": description }));
        self
    }
    /// プレーンテキスト(または `content_type` 指定のテキスト)のレスポンス。
    /// 例: `response_text(200, "ok", "text/plain")`。JSONではないことが
    /// ドキュメント・TypeScriptクライアント(`string`)・GraphQL(`String`)に出る。
    pub fn response_text(mut self, status: u16, description: &str, content_type: &str) -> Self {
        self.responses.insert(
            status.to_string(),
            json!({ "description": description, "content": { content_type: { "schema": { "type": "string" } } } }),
        );
        self
    }
    pub fn http_method(&self) -> Method {
        self.method
    }
    /// OpenAPI形式(`{id}`)の正規化済みパス。
    pub fn path(&self) -> &str {
        &self.path
    }
    /// ルーター用のパターン(`/items/:id` 形式)。
    pub fn route_pattern(&self) -> String {
        self.path
            .split('/')
            .map(|s| {
                if s.len() > 2 && s.starts_with('{') && s.ends_with('}') {
                    format!(":{}", &s[1..s.len() - 1])
                } else {
                    s.to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join("/")
    }
    /// パスの先頭に `prefix` を付けた操作にする(`Route::nest` 用)。
    pub fn prefixed(mut self, prefix: &str) -> Self {
        let p = prefix.trim_end_matches('/');
        self.path = format!("{p}{}", self.path);
        self
    }
    pub fn operation_id(&self) -> String {
        self.id
            .clone()
            .unwrap_or_else(|| default_operation_id(self.method, &self.path))
    }
    pub(crate) fn route_label(&self) -> String {
        format!("{} {}", self.method.key().to_uppercase(), self.path)
    }
}

/// ドキュメント全体。
#[derive(Debug, Clone)]
pub struct Spec {
    title: String,
    version: String,
    description: String,
    servers: Vec<String>,
    api_key_header: Option<String>,
    ops: Vec<Operation>,
    shared: Vec<(String, String, Value)>,
    shared_schemas: Vec<(String, Value)>,
}

impl Spec {
    pub fn new(title: &str, version: &str) -> Self {
        Self {
            title: title.into(),
            version: version.into(),
            description: String::new(),
            servers: vec![],
            api_key_header: None,
            ops: vec![],
            shared: vec![],
            shared_schemas: vec![],
        }
    }
    pub fn description(mut self, d: &str) -> Self {
        self.description = d.into();
        self
    }
    pub fn server(mut self, url: &str) -> Self {
        self.servers.push(url.into());
        self
    }
    /// APIキー認証(ヘッダ名)。`public()` でない操作に適用される。
    pub fn api_key_header(mut self, name: &str) -> Self {
        self.api_key_header = Some(name.into());
        self
    }
    pub fn op(mut self, op: Operation) -> Self {
        self.ops.push(op);
        self
    }
    /// 認証必須の全操作に付く共通エラー応答(例: 401/429)。
    /// `components.responses.<name>` に1回だけ定義し、各操作は `$ref` する。
    pub fn shared_error(mut self, status: u16, name: &str, description: &str) -> Self {
        self.shared.push((
            status.to_string(),
            name.into(),
            json!({ "description": description }),
        ));
        self
    }
    /// `shared_error` の本文付き版(本文の型 `T` も `components.schemas` へ登録)。
    pub fn shared_error_typed<T: JsonSchema>(
        mut self,
        status: u16,
        name: &str,
        description: &str,
    ) -> Self {
        let (s, defs) = schema_of::<T>();
        self.shared_schemas.extend(defs);
        self.shared.push((
            status.to_string(),
            name.into(),
            json!({ "description": description, "content": { "application/json": { "schema": s } } }),
        ));
        self
    }
    pub fn operations(&self) -> &[Operation] {
        &self.ops
    }
    pub fn title(&self) -> &str {
        &self.title
    }

    /// ドキュメントとして壊れている箇所(ルート/operationIdの重複、
    /// レスポンス未定義、summary未設定等)を返す。空なら問題なし。
    pub fn lint(&self) -> Vec<String> {
        let mut problems = Vec::new();
        let mut seen_ids: BTreeMap<String, String> = BTreeMap::new();
        let mut seen_routes: BTreeMap<String, ()> = BTreeMap::new();
        for op in &self.ops {
            let route = op.route_label();
            if seen_routes.insert(route.clone(), ()).is_some() {
                problems.push(format!("重複したルート: {route}"));
            }
            let id = op.operation_id();
            if let Some(prev) = seen_ids.insert(id.clone(), route.clone()) {
                problems.push(format!("operationIdが重複: {id} ({prev} と {route})"));
            }
            if op.responses.is_empty() {
                problems.push(format!("レスポンス未定義: {route}"));
            }
            if matches!(op.method, Method::Get | Method::Delete) && op.request_body.is_some() {
                problems.push(format!("{route} にリクエストボディ(GET/DELETEでは非推奨)"));
            }
            if op.summary.is_none() {
                problems.push(format!("summary未設定: {route}"));
            }
        }
        problems
    }

    /// 実際のルーター定義にあって、ドキュメントに無いルートを返す
    /// (`:param` / `{param}` どちらの書き方でも照合する)。
    pub fn undocumented<'a>(
        &self,
        routes: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> Vec<String> {
        let documented: Vec<String> = self.ops.iter().map(Operation::route_label).collect();
        routes
            .into_iter()
            .filter_map(|(m, p)| {
                let label = format!("{} {}", m.to_uppercase(), normalize_path(p).0);
                (!documented.contains(&label)).then_some(label)
            })
            .collect()
    }

    /// OpenAPI 3.0.3 文書。
    pub fn build(&self) -> Value {
        let mut schemas: Map<String, Value> = Map::new();
        let mut paths: Map<String, Value> = Map::new();
        for (n, s) in &self.shared_schemas {
            schemas.entry(n.clone()).or_insert_with(|| s.clone());
        }
        for op in &self.ops {
            for (n, s) in &op.schemas {
                schemas.entry(n.clone()).or_insert_with(|| s.clone());
            }
            let mut o = Map::new();
            o.insert("operationId".into(), Value::String(op.operation_id()));
            if let Some(s) = &op.summary {
                o.insert("summary".into(), json!(s));
            }
            if let Some(d) = &op.description {
                o.insert("description".into(), json!(d));
            }
            if !op.tags.is_empty() {
                o.insert("tags".into(), json!(op.tags));
            }
            if op.deprecated {
                o.insert("deprecated".into(), json!(true));
            }
            let mut params: Vec<Value> = op
                .path_params
                .iter()
                .map(|n| json!({ "name": n, "in": "path", "required": true, "schema": { "type": "string" } }))
                .collect();
            params.extend(op.params.iter().cloned());
            if !params.is_empty() {
                o.insert("parameters".into(), Value::Array(params));
            }
            if let Some(b) = &op.request_body {
                o.insert("requestBody".into(), b.clone());
            }
            for (k, v) in &op.extensions {
                o.insert(k.clone(), v.clone());
            }
            let mut responses: Map<String, Value> = op
                .responses
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            if op.secured && self.api_key_header.is_some() {
                o.insert("security".into(), json!([{ "ApiKeyAuth": [] }]));
                for (status, name, _) in &self.shared {
                    responses.entry(status.clone()).or_insert_with(
                        || json!({ "$ref": format!("#/components/responses/{name}") }),
                    );
                }
                responses
                    .entry("401".to_string())
                    .or_insert_with(|| json!({ "description": "Missing or invalid API key" }));
            } else {
                o.insert("security".into(), json!([]));
            }
            o.insert("responses".into(), Value::Object(responses));
            if let Some(item) = paths
                .entry(op.path.clone())
                .or_insert_with(|| json!({}))
                .as_object_mut()
            {
                item.insert(op.method.key().into(), Value::Object(o));
            }
        }
        let mut info = json!({ "title": self.title, "version": self.version });
        if !self.description.is_empty() {
            info["description"] = json!(self.description);
        }
        let mut doc = json!({
            "openapi": "3.0.3",
            "info": info,
            "paths": paths,
            "components": { "schemas": schemas },
        });
        if !self.servers.is_empty() {
            doc["servers"] =
                Value::Array(self.servers.iter().map(|u| json!({ "url": u })).collect());
        }
        if !self.shared.is_empty() {
            let r: Map<String, Value> = self
                .shared
                .iter()
                .map(|(_, n, v)| (n.clone(), v.clone()))
                .collect();
            doc["components"]["responses"] = Value::Object(r);
        }
        if let Some(h) = &self.api_key_header {
            doc["components"]["securitySchemes"] =
                json!({ "ApiKeyAuth": { "type": "apiKey", "in": "header", "name": h } });
        }
        doc
    }
}

/// 表示用UIの種類。
#[derive(Debug, Clone, Copy)]
pub enum DocsUi {
    SwaggerUi,
    Redoc,
}

/// `spec_url` のOpenAPI文書を表示するHTMLを返す(poem-openapiの
/// `swagger_ui()` / `redoc()` 相当)。UIの部品はCDNから読み込む。
pub fn docs_html(ui: DocsUi, title: &str, spec_url: &str) -> String {
    let title = title.replace('&', "&amp;").replace('<', "&lt;");
    let url = serde_json::to_string(spec_url)
        .unwrap_or_else(|_| "\"\"".into())
        .replace('<', "\\u003c");
    match ui {
        DocsUi::SwaggerUi => format!(
            "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{title}</title>\
<link rel=\"stylesheet\" href=\"https://cdn.jsdelivr.net/npm/swagger-ui-dist@5.17.14/swagger-ui.css\"></head>\
<body><div id=\"ui\"></div><script src=\"https://cdn.jsdelivr.net/npm/swagger-ui-dist@5.17.14/swagger-ui-bundle.js\"></script>\
<script>SwaggerUIBundle({{url:{url},dom_id:'#ui'}})</script></body></html>"
        ),
        DocsUi::Redoc => format!(
            "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{title}</title></head>\
<body><div id=\"r\"></div><script src=\"https://cdn.jsdelivr.net/npm/redoc@2.1.5/bundles/redoc.standalone.js\"></script>\
<script>Redoc.init({url},{{}},document.getElementById('r'))</script></body></html>"
        ),
    }
}
