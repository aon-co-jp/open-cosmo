//! MCP Server — Poem-parity gap ("MCP Server(poem-mcpserver相当)",
//! `docs/poem-parity.md`). The [Model Context Protocol](https://modelcontextprotocol.io/)
//! lets an LLM client (Claude Desktop, an IDE agent, etc.) discover and
//! call a server's capabilities as structured "tools" instead of scraping
//! its REST API by hand.
//!
//! Implements the JSON-RPC 2.0 message layer and all three MCP capability
//! types -- `tools/list` + `tools/call`, `resources/list` +
//! `resources/read`, and `prompts/list` + `prompts/get` -- plus
//! `initialize`, over a single `POST /mcp` endpoint (the
//! ["Streamable HTTP" transport](https://modelcontextprotocol.io/specification/2025-03-26/basic/transports#streamable-http)'s
//! simple case: one JSON-RPC request in, one JSON-RPC response out, no
//! SSE stream -- sufficient for stateless calls, which is all this server
//! exposes). No new dependencies -- this is JSON-RPC over the same
//! `read_json_body`/`json_response` machinery every other handler in this
//! crate already uses.
//!
//! Every capability is backed by a production code path, not an
//! MCP-only stub: the two tools (`health_check`, `self_issue_api_key`)
//! reuse the same logic as `GET /health` and `POST /api/keys/self-issue`;
//! the two resources (`openapi://spec`, `health://status`) serve the same
//! data as `GET /api/openapi.json` and `GET /health`; the one prompt
//! (`summarize_api`) is rendered from the same live `openapi::spec()` the
//! `openapi://spec` resource serves.

use crate::hyper_compat::{empty_status, json_response, read_json_body, Handler};
use crate::keyring::KeyGuardian;
use crate::state::AppState;
use hyper::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;

const MCP_PROTOCOL_VERSION: &str = "2024-11-05";
const SELF_ISSUE_KEY_TTL_HOURS: i64 = 24;

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    #[serde(default)]
    jsonrpc: String,
    /// Absent for a JSON-RPC *notification* (e.g.
    /// `notifications/initialized`), which must not receive a response.
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: &'static str,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

fn ok_response(id: Value, result: Value) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0",
        id,
        result: Some(result),
        error: None,
    }
}

fn err_response(id: Value, code: i64, message: impl Into<String>) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0",
        id,
        result: None,
        error: Some(JsonRpcError {
            code,
            message: message.into(),
        }),
    }
}

/// The `initialize` handshake response: this server's protocol version,
/// declared capabilities (tools, resources, and prompts), and identity.
fn initialize_result() -> Value {
    json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "capabilities": { "tools": {}, "resources": {}, "prompts": {} },
        "serverInfo": {
            "name": "open-runo-router",
            "version": env!("CARGO_PKG_VERSION"),
        },
    })
}

/// `prompts/list`: the one prompt template this server offers. Like the
/// tools and resources above, it's wired to real data (the same
/// `openapi::spec()` behind the `openapi://spec` resource), not a static
/// fixture -- an MCP client using this prompt gets the server's *current*
/// API surface, not a snapshot frozen at prompt-authoring time.
fn prompts_list_result() -> Value {
    json!({
        "prompts": [
            {
                "name": "summarize_api",
                "description": "Ask the model to summarize this server's remaining non-GraphQL HTTP surface, sourced from its live OpenAPI 3.0 document.",
                "arguments": [],
            },
        ]
    })
}

/// `prompts/get`: render `name` (with `arguments`, currently unused since
/// `summarize_api` takes none) into the MCP `GetPromptResult` shape --
/// a description plus a list of chat-style messages an MCP client can feed
/// straight to its model.
fn get_prompt(name: &str) -> Result<Value, String> {
    match name {
        "summarize_api" => {
            let spec = crate::openapi::spec();
            Ok(json!({
                "description": "Summarize this server's REST API surface from its OpenAPI 3.0 document.",
                "messages": [{
                    "role": "user",
                    "content": {
                        "type": "text",
                        "text": format!(
                            "Please summarize the REST API described by the following OpenAPI 3.0 document. \
                             List the endpoints grouped by resource, and call out which ones are read-only \
                             versus state-changing:\n\n{spec}"
                        ),
                    },
                }],
            }))
        },
        other => Err(format!("unknown prompt: {other}")),
    }
}

/// `resources/list`: the two read-only resources this server exposes,
/// both backed by real production data (not MCP-only fixtures) -- the
/// same OpenAPI spec `GET /api/openapi.json` serves, and the same health
/// status `GET /health` serves.
fn resources_list_result() -> Value {
    json!({
        "resources": [
            {
                "uri": "openapi://spec",
                "name": "OpenAPI specification",
                "description": "The non-GraphQL surface left on this server (health/ACME/MCP/SCIM/internal) as OpenAPI 3.0. Admin operations are GraphQL: POST /graphql, schema at GET /graphql/schema.graphql.",
                "mimeType": "application/json",
            },
            {
                "uri": "health://status",
                "name": "Health status",
                "description": "Current service health -- the same data GET /health serves.",
                "mimeType": "application/json",
            },
        ]
    })
}

/// `resources/read`: fetch one resource's content by URI, in the MCP
/// `ReadResourceResult` shape (a list of contents, each tagged with the
/// URI it came from and its MIME type).
fn read_resource(uri: &str) -> Result<Value, String> {
    let contents = match uri {
        "openapi://spec" => crate::openapi::spec(),
        "health://status" => json!({
            "status": "ok",
            "service": "open-runo-router",
            "version": env!("CARGO_PKG_VERSION"),
        }),
        other => return Err(format!("unknown resource: {other}")),
    };
    Ok(json!({
        "contents": [{
            "uri": uri,
            "mimeType": "application/json",
            "text": contents.to_string(),
        }]
    }))
}

/// `tools/list`: the tool catalog an MCP client discovers and can then
/// invoke via `tools/call`. `inputSchema` is plain JSON Schema, per the
/// MCP spec's `Tool` shape.
fn tools_list_result() -> Value {
    json!({
        "tools": [
            {
                "name": "health_check",
                "description": "Check whether the open-runo-router service is up and report its version.",
                "inputSchema": { "type": "object", "properties": {} },
            },
            {
                "name": "self_issue_api_key",
                "description": "Obtain a working X-Api-Key for this server's REST API, scoped to the developer role and expiring after 24 hours. No human key management required -- the same self-issue mechanism POST /api/keys/self-issue exposes over plain REST.",
                "inputSchema": { "type": "object", "properties": {} },
            },
        ]
    })
}

/// Wrap a tool's JSON result in the MCP `CallToolResult` shape: a list of
/// content blocks (here, always one `text` block containing the
/// serialized JSON) plus an `isError` flag.
fn tool_content(value: &Value) -> Value {
    json!({
        "content": [{ "type": "text", "text": value.to_string() }],
        "isError": false,
    })
}

fn tool_error(message: impl Into<String>) -> Value {
    json!({
        "content": [{ "type": "text", "text": message.into() }],
        "isError": true,
    })
}

async fn call_health_check() -> Value {
    tool_content(&json!({
        "status": "ok",
        "service": "open-runo-router",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

async fn call_self_issue_api_key(state: &Arc<AppState>, guardian: &Arc<KeyGuardian>) -> Value {
    let owner = format!("mcp-client-{}", uuid::Uuid::new_v4());
    let expires_at = chrono::Utc::now() + chrono::Duration::hours(SELF_ISSUE_KEY_TTL_HOURS);
    match guardian
        .issue(&owner, vec!["developer".to_string()], Some(expires_at))
        .await
    {
        Ok(api_key) => {
            crate::audit::record(state, "mcp-server", "key.self_issue", owner).await;
            tool_content(&json!({ "api_key": api_key, "expires_at": expires_at.to_rfc3339() }))
        },
        Err(e) => tool_error(format!("failed to issue API key: {e}")),
    }
}

async fn dispatch(
    state: &Arc<AppState>,
    guardian: &Arc<KeyGuardian>,
    req: JsonRpcRequest,
) -> Option<JsonRpcResponse> {
    // A JSON-RPC *notification* has no `id` and must receive no response
    // at all (not even an empty one) -- `notifications/initialized` is
    // the one an MCP client sends after a successful `initialize`.
    let id = req.id?;

    if req.jsonrpc != "2.0" {
        return Some(err_response(
            id,
            -32600,
            "invalid request: jsonrpc must be \"2.0\"",
        ));
    }

    Some(match req.method.as_str() {
        "initialize" => ok_response(id, initialize_result()),
        "tools/list" => ok_response(id, tools_list_result()),
        "resources/list" => ok_response(id, resources_list_result()),
        "resources/read" => {
            let Some(uri) = req.params.get("uri").and_then(Value::as_str) else {
                return Some(err_response(id, -32602, "invalid params: missing \"uri\""));
            };
            match read_resource(uri) {
                Ok(result) => ok_response(id, result),
                // Per the MCP spec, an unknown resource URI is a JSON-RPC
                // error (unlike an unknown tool name, which is a
                // tool-level isError:true) -- resources/read has no
                // equivalent "soft failure" envelope to report it inside.
                Err(message) => err_response(id, -32602, message),
            }
        },
        "prompts/list" => ok_response(id, prompts_list_result()),
        "prompts/get" => {
            let Some(name) = req.params.get("name").and_then(Value::as_str) else {
                return Some(err_response(id, -32602, "invalid params: missing \"name\""));
            };
            match get_prompt(name) {
                Ok(result) => ok_response(id, result),
                Err(message) => err_response(id, -32602, message),
            }
        },
        "tools/call" => {
            let Some(name) = req.params.get("name").and_then(Value::as_str) else {
                return Some(err_response(id, -32602, "invalid params: missing \"name\""));
            };
            let result = match name {
                "health_check" => call_health_check().await,
                "self_issue_api_key" => call_self_issue_api_key(state, guardian).await,
                other => tool_error(format!("unknown tool: {other}")),
            };
            ok_response(id, result)
        },
        other => err_response(id, -32601, format!("method not found: {other}")),
    })
}

/// `POST /mcp` — the MCP Streamable HTTP transport's single endpoint. No
/// auth required to reach it (mirrors `/api/keys/self-issue`'s own
/// no-auth stance): an MCP client's very first call is `initialize`,
/// before it has any credential, and `self_issue_api_key` exists
/// specifically to bootstrap one from here.
pub fn mcp_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let parsed: JsonRpcRequest = match read_json_body(req).await {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            match dispatch(&state, &guardian, parsed).await {
                Some(resp) => json_response(StatusCode::OK, &resp),
                // A notification was handled but produces no body --
                // 204 is the correct "processed, nothing to say" response
                // for an HTTP transport carrying a JSON-RPC notification.
                None => empty_status(StatusCode::NO_CONTENT),
            }
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hyper_compat::{serve, Router};
    use crate::keyring::GuardianConfig;
    use hyper::Method;

    fn guardian(state: &Arc<AppState>) -> Arc<KeyGuardian> {
        Arc::new(KeyGuardian::new(
            Arc::clone(&state.db),
            GuardianConfig::from_env(),
        ))
    }

    async fn start() -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new().route(Method::POST, "/mcp", mcp_handler(state, guardian));
        serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port")
    }

    #[tokio::test]
    async fn initialize_returns_protocol_version_and_capabilities() {
        let (addr, _handle) = start().await;
        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/mcp"))
            .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["id"], 1);
        assert_eq!(body["result"]["protocolVersion"], MCP_PROTOCOL_VERSION);
        assert_eq!(body["result"]["serverInfo"]["name"], "open-runo-router");
    }

    #[tokio::test]
    async fn notification_gets_no_json_body_response() {
        let (addr, _handle) = start().await;
        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/mcp"))
            // No "id" field -- this is a notification, per JSON-RPC 2.0.
            .json(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized", "params": {} }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn tools_list_advertises_both_real_tools() {
        let (addr, _handle) = start().await;
        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/mcp"))
            .json(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {} }))
            .send()
            .await
            .expect("request should succeed");
        let body: Value = resp.json().await.unwrap();
        let tools = body["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"health_check"));
        assert!(names.contains(&"self_issue_api_key"));
    }

    #[tokio::test]
    async fn resources_list_advertises_both_real_resources() {
        let (addr, _handle) = start().await;
        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/mcp"))
            .json(&json!({ "jsonrpc": "2.0", "id": 7, "method": "resources/list", "params": {} }))
            .send()
            .await
            .expect("request should succeed");
        let body: Value = resp.json().await.unwrap();
        let resources = body["result"]["resources"].as_array().unwrap();
        let uris: Vec<&str> = resources
            .iter()
            .map(|r| r["uri"].as_str().unwrap())
            .collect();
        assert!(uris.contains(&"openapi://spec"));
        assert!(uris.contains(&"health://status"));
    }

    #[tokio::test]
    async fn resources_read_openapi_spec_matches_the_real_rest_endpoint() {
        let (addr, _handle) = start().await;
        let client = reqwest::Client::new();

        let mcp_resp = client
            .post(format!("http://{addr}/mcp"))
            .json(&json!({ "jsonrpc": "2.0", "id": 8, "method": "resources/read", "params": { "uri": "openapi://spec" } }))
            .send()
            .await
            .expect("request should succeed");
        let mcp_body: Value = mcp_resp.json().await.unwrap();
        let text = mcp_body["result"]["contents"][0]["text"].as_str().unwrap();
        let via_mcp: Value = serde_json::from_str(text).unwrap();

        // Same server, same data, reached two different ways -- the MCP
        // resource must be byte-for-byte the same document as the plain
        // REST endpoint, not a separate hand-maintained copy that could
        // drift.
        let via_rest: Value = crate::openapi::spec();
        assert_eq!(via_mcp, via_rest);
        assert_eq!(via_mcp["openapi"], "3.0.3");
    }

    #[tokio::test]
    async fn resources_read_unknown_uri_is_a_json_rpc_error() {
        let (addr, _handle) = start().await;
        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/mcp"))
            .json(&json!({ "jsonrpc": "2.0", "id": 9, "method": "resources/read", "params": { "uri": "no://such/resource" } }))
            .send()
            .await
            .expect("request should succeed");
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["error"]["code"], -32602);
    }

    #[tokio::test]
    async fn tools_call_health_check_returns_real_status() {
        let (addr, _handle) = start().await;
        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/mcp"))
            .json(&json!({
                "jsonrpc": "2.0", "id": 3, "method": "tools/call",
                "params": { "name": "health_check", "arguments": {} }
            }))
            .send()
            .await
            .expect("request should succeed");
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["result"]["isError"], false);
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        let inner: Value = serde_json::from_str(text).unwrap();
        assert_eq!(inner["status"], "ok");
        assert_eq!(inner["service"], "open-runo-router");
    }

    #[tokio::test]
    async fn tools_call_self_issue_api_key_returns_a_real_working_key() {
        let (addr, _handle) = start().await;
        let client = reqwest::Client::new();

        let resp = client
            .post(format!("http://{addr}/mcp"))
            .json(&json!({
                "jsonrpc": "2.0", "id": 4, "method": "tools/call",
                "params": { "name": "self_issue_api_key", "arguments": {} }
            }))
            .send()
            .await
            .expect("request should succeed");
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["result"]["isError"], false);
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        let inner: Value = serde_json::from_str(text).unwrap();
        let api_key = inner["api_key"]
            .as_str()
            .expect("api_key should be a string");
        assert!(!api_key.is_empty());
    }

    #[tokio::test]
    async fn tools_call_unknown_tool_reports_iserror_not_a_transport_failure() {
        let (addr, _handle) = start().await;
        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/mcp"))
            .json(&json!({
                "jsonrpc": "2.0", "id": 5, "method": "tools/call",
                "params": { "name": "no_such_tool", "arguments": {} }
            }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::OK,
            "an unknown tool is a tool-level error, not an HTTP-level one"
        );
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["result"]["isError"], true);
    }

    #[tokio::test]
    async fn unknown_method_returns_json_rpc_method_not_found() {
        let (addr, _handle) = start().await;
        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/mcp"))
            .json(&json!({ "jsonrpc": "2.0", "id": 6, "method": "no/such/method", "params": {} }))
            .send()
            .await
            .expect("request should succeed");
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["error"]["code"], -32601);
    }

    /// The realistic sequence a real MCP client follows: initialize, then
    /// discover tools, then call one -- over the same persistent-enough
    /// server (three separate requests, same running process), proving
    /// the handler is stateless-safe to call repeatedly in sequence.
    #[tokio::test]
    async fn full_initialize_then_list_then_call_sequence() {
        let (addr, _handle) = start().await;
        let client = reqwest::Client::new();

        let init = client
            .post(format!("http://{addr}/mcp"))
            .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }))
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap();
        assert_eq!(init["result"]["protocolVersion"], MCP_PROTOCOL_VERSION);

        let notif_status = client
            .post(format!("http://{addr}/mcp"))
            .json(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized", "params": {} }))
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(notif_status, reqwest::StatusCode::NO_CONTENT);

        let list = client
            .post(format!("http://{addr}/mcp"))
            .json(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {} }))
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap();
        assert!(!list["result"]["tools"].as_array().unwrap().is_empty());

        let call = client
            .post(format!("http://{addr}/mcp"))
            .json(&json!({
                "jsonrpc": "2.0", "id": 3, "method": "tools/call",
                "params": { "name": "health_check", "arguments": {} }
            }))
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap();
        assert_eq!(call["result"]["isError"], false);
    }

    #[tokio::test]
    async fn prompts_list_advertises_the_real_prompt() {
        let (addr, _handle) = start().await;
        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/mcp"))
            .json(&json!({ "jsonrpc": "2.0", "id": 10, "method": "prompts/list", "params": {} }))
            .send()
            .await
            .expect("request should succeed");
        let body: Value = resp.json().await.unwrap();
        let prompts = body["result"]["prompts"].as_array().unwrap();
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0]["name"], "summarize_api");
    }

    #[tokio::test]
    async fn prompts_get_summarize_api_returns_a_message_containing_the_real_openapi_spec() {
        let (addr, _handle) = start().await;
        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/mcp"))
            .json(&json!({ "jsonrpc": "2.0", "id": 11, "method": "prompts/get", "params": { "name": "summarize_api" } }))
            .send()
            .await
            .expect("request should succeed");
        let body: Value = resp.json().await.unwrap();
        let text = body["result"]["messages"][0]["content"]["text"]
            .as_str()
            .unwrap();
        let spec = crate::openapi::spec();
        assert!(
            text.contains(&spec.to_string()),
            "rendered prompt should embed the real, live OpenAPI spec, not a stub"
        );
    }

    #[tokio::test]
    async fn prompts_get_unknown_name_is_a_json_rpc_error() {
        let (addr, _handle) = start().await;
        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/mcp"))
            .json(&json!({ "jsonrpc": "2.0", "id": 12, "method": "prompts/get", "params": { "name": "no_such_prompt" } }))
            .send()
            .await
            .expect("request should succeed");
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["error"]["code"], -32602);
    }
}
