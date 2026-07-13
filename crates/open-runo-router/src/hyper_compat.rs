//! Poem-free HTTP foundation: hand-rolled router + response helpers on top
//! of `hyper` directly. New handlers are migrated onto this module one at a
//! time; once every handler and middleware has moved here, `poem` is
//! dropped from this crate's `Cargo.toml` entirely.
//!
//! Design (see CLAUDE.md HANDOFF for the full migration plan):
//! - Handlers are plain `async fn(Request) -> Response` closures capturing
//!   `Arc<AppState>` etc. — no `#[handler]` macro, no `Endpoint` trait.
//! - Middleware is "function in, function out" composition, not a trait
//!   hierarchy.
//! - Routing is a small path+method table with manual `:param` matching
//!   (no external router crate needed yet at this scale).

use bytes::Bytes;
use futures::Stream;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::{Method, Request as HyperRequest, Response as HyperResponse, StatusCode};
use std::collections::HashMap;
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Boxed so both fixed bodies (`json_response`) and streamed ones
/// (`sse_response`) can share a single `Response` type.
pub type Body = BoxBody<Bytes, Infallible>;
pub type Request = HyperRequest<Incoming>;
pub type Response = HyperResponse<Body>;
pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;
pub type Handler = Arc<dyn Fn(Request, Params) -> BoxFuture<Response> + Send + Sync>;

/// Path parameters extracted from a matched route, e.g. `:table` → value.
#[derive(Debug, Default, Clone)]
pub struct Params(pub HashMap<String, String>);

impl Params {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(|s| s.as_str())
    }
}

/// Wrap raw bytes as a fixed (non-streaming) [`Body`]. `pub` so other
/// modules (e.g. `middleware_hyper::with_compression`, which needs to
/// substitute a gzip-compressed byte buffer for an existing response body)
/// can build a `Body` without duplicating the `Full`/`boxed()` plumbing.
pub fn fixed_body(bytes: Bytes) -> Body {
    Full::new(bytes).map_err(|never| match never {}).boxed()
}

/// Build a JSON response with the given status code.
pub fn json_response(status: StatusCode, value: &impl serde::Serialize) -> Response {
    let body = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
    HyperResponse::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(fixed_body(Bytes::from(body)))
        .expect("building a response from a fixed set of valid headers cannot fail")
}

pub fn empty_status(status: StatusCode) -> Response {
    HyperResponse::builder()
        .status(status)
        .body(fixed_body(Bytes::new()))
        .expect("building a response from a fixed set of valid headers cannot fail")
}

/// Build an `text/html; charset=utf-8` response (e.g. for GraphiQL).
pub fn html_response(status: StatusCode, html: impl Into<String>) -> Response {
    HyperResponse::builder()
        .status(status)
        .header("content-type", "text/html; charset=utf-8")
        .body(fixed_body(Bytes::from(html.into())))
        .expect("building a response from a fixed set of valid headers cannot fail")
}

/// One Server-Sent Event: an optional `event:` type and its `data:` payload.
pub struct SseEvent {
    pub event_type: Option<&'static str>,
    pub data: String,
}

impl SseEvent {
    fn encode(&self) -> Bytes {
        let mut out = String::new();
        if let Some(ty) = self.event_type {
            out.push_str("event: ");
            out.push_str(ty);
            out.push('\n');
        }
        for line in self.data.split('\n') {
            out.push_str("data: ");
            out.push_str(line);
            out.push('\n');
        }
        out.push('\n');
        Bytes::from(out)
    }
}

/// Build a `text/event-stream` response from a stream of [`SseEvent`]s.
/// Poem-free equivalent of `poem::web::sse::SSE`.
pub fn sse_response<S>(stream: S) -> Response
where
    S: Stream<Item = SseEvent> + Send + Sync + 'static,
{
    use futures::StreamExt;
    let frame_stream = stream.map(|event| Ok::<_, Infallible>(Frame::data(event.encode())));
    let body: Body = BodyExt::boxed(StreamBody::new(frame_stream));
    HyperResponse::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .body(body)
        .expect("building a response from a fixed set of valid headers cannot fail")
}

// ── Generic WebSocket support (RFC 6455) ─────────────────────────────────
//
// Poem-free equivalent of `poem::web::websocket::WebSocket`. Hand-rolled
// from scratch (no WebSocket-framework crate) on top of hyper's raw
// `Request::extensions()` / `hyper::upgrade::on` mechanism -- the same
// primitive real Poem itself is built on. The only crate used here is
// `sha1` (a narrow, single-purpose hash primitive with the same shape as
// the already-approved `sha2`/`hex`/`jsonwebtoken`), for the
// `Sec-WebSocket-Accept` handshake hash; frame parsing/writing and base64
// encoding are hand-written below.
//
// This is deliberately additive: it does not touch the poem-based
// GraphQL Subscriptions path in `open-runo-gateway`'s `graphql_route`
// (which keeps using `async-graphql-poem`), nor the SSE transport above.

use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// The fixed GUID from RFC 6455 §1.3, concatenated with the client's
/// `Sec-WebSocket-Key` before hashing to produce `Sec-WebSocket-Accept`.
const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// Minimal base64 (standard alphabet, padded) encoder -- RFC 6455 requires
/// `Sec-WebSocket-Accept` to be base64, and pulling in a whole `base64`
/// crate for one 20-byte SHA-1 digest isn't worth a new dependency.
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied();
        let b2 = chunk.get(2).copied();
        out.push(ALPHABET[(b0 >> 2) as usize] as char);
        out.push(ALPHABET[(((b0 & 0x03) << 4) | (b1.unwrap_or(0) >> 4)) as usize] as char);
        if let Some(b1) = b1 {
            out.push(ALPHABET[(((b1 & 0x0f) << 2) | (b2.unwrap_or(0) >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if let Some(b2) = b2 {
            out.push(ALPHABET[(b2 & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// Compute `Sec-WebSocket-Accept` from the client's `Sec-WebSocket-Key`.
fn accept_key(client_key: &str) -> String {
    use sha1::{Digest, Sha1};
    let mut hasher = Sha1::new();
    hasher.update(client_key.as_bytes());
    hasher.update(WS_GUID.as_bytes());
    base64_encode(&hasher.finalize())
}

/// Validate the RFC 6455 upgrade request headers and, if valid, return the
/// computed `Sec-WebSocket-Accept` value.
fn validate_upgrade_request(req: &Request) -> Option<String> {
    let headers = req.headers();
    let header_has_token = |name: &str, token: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.split(',').any(|part| part.trim().eq_ignore_ascii_case(token)))
            .unwrap_or(false)
    };

    if !header_has_token("upgrade", "websocket") {
        return None;
    }
    if !header_has_token("connection", "upgrade") {
        return None;
    }
    let version_ok = headers
        .get("sec-websocket-version")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim() == "13")
        .unwrap_or(false);
    if !version_ok {
        return None;
    }
    let key = headers.get("sec-websocket-key")?.to_str().ok()?;
    Some(accept_key(key))
}

/// One decoded WebSocket message (fragmentation is transparently
/// reassembled by [`WebSocketConnection::recv`]; control frames like
/// ping/pong/close are handled internally and never surfaced here).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsMessage {
    Text(String),
    Binary(Vec<u8>),
}

const OP_CONTINUATION: u8 = 0x0;
const OP_TEXT: u8 = 0x1;
const OP_BINARY: u8 = 0x2;
const OP_CLOSE: u8 = 0x8;
const OP_PING: u8 = 0x9;
const OP_PONG: u8 = 0xA;

struct RawFrame {
    fin: bool,
    opcode: u8,
    payload: Vec<u8>,
}

/// Read one raw frame off the wire, unmasking client→server payloads per
/// RFC 6455 §5.2/§5.3. Returns `None` on clean EOF.
async fn read_frame<R: AsyncReadExt + Unpin>(io: &mut R) -> std::io::Result<Option<RawFrame>> {
    let mut head = [0u8; 2];
    if io.read_exact(&mut head).await.is_err() {
        return Ok(None);
    }
    let fin = head[0] & 0x80 != 0;
    let opcode = head[0] & 0x0f;
    let masked = head[1] & 0x80 != 0;
    let mut len = (head[1] & 0x7f) as u64;

    if len == 126 {
        let mut ext = [0u8; 2];
        io.read_exact(&mut ext).await?;
        len = u16::from_be_bytes(ext) as u64;
    } else if len == 127 {
        let mut ext = [0u8; 8];
        io.read_exact(&mut ext).await?;
        len = u64::from_be_bytes(ext);
    }

    let mask = if masked {
        let mut m = [0u8; 4];
        io.read_exact(&mut m).await?;
        Some(m)
    } else {
        None
    };

    let mut payload = vec![0u8; len as usize];
    if len > 0 {
        io.read_exact(&mut payload).await?;
    }
    if let Some(mask) = mask {
        for (i, byte) in payload.iter_mut().enumerate() {
            *byte ^= mask[i % 4];
        }
    }

    Ok(Some(RawFrame { fin, opcode, payload }))
}

/// Write one raw, unmasked frame (server→client frames must never be
/// masked per RFC 6455 §5.1).
async fn write_frame<W: AsyncWriteExt + Unpin>(
    io: &mut W,
    opcode: u8,
    payload: &[u8],
) -> std::io::Result<()> {
    let mut buf = Vec::with_capacity(payload.len() + 10);
    buf.push(0x80 | opcode); // FIN=1, no fragmentation on the server side.
    let len = payload.len();
    if len < 126 {
        buf.push(len as u8);
    } else if len <= u16::MAX as usize {
        buf.push(126);
        buf.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        buf.push(127);
        buf.extend_from_slice(&(len as u64).to_be_bytes());
    }
    buf.extend_from_slice(payload);
    io.write_all(&buf).await?;
    io.flush().await
}

/// A live, upgraded WebSocket connection handed to a handler's callback.
/// Poem-free equivalent of Poem's `WebSocketStream` -- basic send/receive
/// of text/binary frames, with ping/pong/close handled transparently.
pub struct WebSocketConnection {
    io: TokioIo<Upgraded>,
}

impl WebSocketConnection {
    /// Receive the next application message (`Text`/`Binary`), transparently
    /// answering `Ping` with `Pong` and swallowing unsolicited `Pong`.
    /// Returns `None` once the peer sends `Close` or the connection drops;
    /// a `Close` frame is echoed back before returning, per RFC 6455 §5.5.1.
    pub async fn recv(&mut self) -> Option<WsMessage> {
        let mut fragments: Vec<u8> = Vec::new();
        let mut fragment_opcode = OP_CONTINUATION;
        loop {
            let frame = match read_frame(&mut self.io).await {
                Ok(Some(f)) => f,
                _ => return None,
            };
            match frame.opcode {
                OP_PING => {
                    let _ = write_frame(&mut self.io, OP_PONG, &frame.payload).await;
                    continue;
                }
                OP_PONG => continue,
                OP_CLOSE => {
                    let _ = write_frame(&mut self.io, OP_CLOSE, &frame.payload).await;
                    return None;
                }
                OP_TEXT | OP_BINARY => {
                    fragment_opcode = frame.opcode;
                    fragments = frame.payload;
                }
                OP_CONTINUATION => {
                    fragments.extend_from_slice(&frame.payload);
                }
                _ => continue, // unknown opcode: ignore rather than tear down.
            }
            if frame.fin {
                return match fragment_opcode {
                    OP_TEXT => Some(WsMessage::Text(String::from_utf8_lossy(&fragments).into_owned())),
                    OP_BINARY => Some(WsMessage::Binary(fragments)),
                    _ => continue,
                };
            }
        }
    }

    pub async fn send_text(&mut self, text: impl AsRef<str>) -> std::io::Result<()> {
        write_frame(&mut self.io, OP_TEXT, text.as_ref().as_bytes()).await
    }

    pub async fn send_binary(&mut self, data: &[u8]) -> std::io::Result<()> {
        write_frame(&mut self.io, OP_BINARY, data).await
    }

    /// Send a `Close` frame. The peer's own `Close` reply (if any) is
    /// consumed by whichever side calls `recv()` next.
    pub async fn close(&mut self) -> std::io::Result<()> {
        write_frame(&mut self.io, OP_CLOSE, &[]).await
    }
}

/// Build a [`Handler`] that performs the RFC 6455 handshake and, on
/// success, hands the caller-supplied closure a live [`WebSocketConnection`]
/// once the underlying TCP connection has actually been upgraded. Poem-free
/// equivalent of Poem's `WebSocket` extractor + `.on_upgrade(...)`.
///
/// Requests that fail handshake validation (missing/incorrect
/// `Upgrade`/`Connection`/`Sec-WebSocket-Key`/`Sec-WebSocket-Version`
/// headers) get a plain `400 Bad Request` and the closure is never called.
pub fn websocket_handler<F>(f: F) -> Handler
where
    F: Fn(WebSocketConnection) -> BoxFuture<()> + Send + Sync + 'static,
{
    let f = Arc::new(f);
    Arc::new(move |mut req: Request, _params: Params| {
        let f = Arc::clone(&f);
        Box::pin(async move {
            let Some(accept) = validate_upgrade_request(&req) else {
                return json_response(
                    StatusCode::BAD_REQUEST,
                    &serde_json::json!({ "error": "invalid WebSocket upgrade request" }),
                );
            };

            // `hyper::upgrade::on` must be called on the *server-observed*
            // request before the response is returned; the returned future
            // only resolves after this handler's response has actually been
            // flushed back to the client, so it's spawned as its own task
            // rather than awaited inline here.
            let upgrade_fut = hyper::upgrade::on(&mut req);
            tokio::spawn(async move {
                match upgrade_fut.await {
                    Ok(upgraded) => {
                        let conn = WebSocketConnection { io: TokioIo::new(upgraded) };
                        f(conn).await;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "WebSocket upgrade failed");
                    }
                }
            });

            HyperResponse::builder()
                .status(StatusCode::SWITCHING_PROTOCOLS)
                .header("upgrade", "websocket")
                .header("connection", "Upgrade")
                .header("sec-websocket-accept", accept)
                .body(fixed_body(Bytes::new()))
                .expect("building a response from a fixed set of valid headers cannot fail")
        })
    })
}

/// Parse the request's `?a=1&b=2` query string into a lookup map. Minimal
/// percent-decoding (`%XX`, `+` → space) — no external query-string crate
/// needed at this scale.
pub fn query_params(req: &Request) -> HashMap<String, String> {
    let mut params = HashMap::new();
    let Some(query) = req.uri().query() else {
        return params;
    };
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        params.insert(percent_decode(key), percent_decode(value));
    }
    params
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                if let Ok(byte) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    out.push(byte);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub async fn read_json_body<T: serde::de::DeserializeOwned>(
    req: Request,
) -> Result<T, Response> {
    let bytes = match req.into_body().collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(_) => {
            return Err(json_response(
                StatusCode::BAD_REQUEST,
                &serde_json::json!({ "error": "failed to read request body" }),
            ))
        }
    };
    serde_json::from_slice::<T>(&bytes).map_err(|e| {
        json_response(
            StatusCode::BAD_REQUEST,
            &serde_json::json!({ "error": format!("invalid JSON body: {e}") }),
        )
    })
}

// ── Multipart/form-data support (RFC 7578) ───────────────────────────────
//
// Poem-free equivalent of Poem's `Multipart` extractor. Hand-rolled byte
// scanning over the collected request body -- no `multer`/`multipart`
// crate dependency, matching this module's existing pattern for
// self-contained protocol parsing (see the WebSocket section above).

/// One decoded part of a `multipart/form-data` request: the form field
/// `name` from its `Content-Disposition` header, an optional `filename`
/// (present for file inputs), an optional part-level `Content-Type`, and
/// the raw bytes of the part body.
#[derive(Debug, Clone)]
pub struct MultipartField {
    pub name: String,
    pub filename: Option<String>,
    pub content_type: Option<String>,
    pub data: Vec<u8>,
}

/// Extract the `boundary=...` parameter from a `Content-Type:
/// multipart/form-data; boundary=...` header value.
fn multipart_boundary(content_type: &str) -> Option<String> {
    content_type.split(';').find_map(|part| {
        let part = part.trim();
        part.strip_prefix("boundary=")
            .map(|b| b.trim_matches('"').to_string())
    })
}

fn find_subslice(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from > haystack.len() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

/// Parse a `Content-Disposition: form-data; name="..."; filename="..."`
/// header line into `(name, filename)`.
fn parse_content_disposition(line: &str) -> (Option<String>, Option<String>) {
    let mut name = None;
    let mut filename = None;
    for piece in line.split(';').skip(1) {
        let piece = piece.trim();
        if let Some(v) = piece.strip_prefix("name=") {
            name = Some(v.trim_matches('"').to_string());
        } else if let Some(v) = piece.strip_prefix("filename=") {
            filename = Some(v.trim_matches('"').to_string());
        }
    }
    (name, filename)
}

/// Read and parse a `multipart/form-data` request body into its constituent
/// fields. Returns a `400`-shaped `Err(Response)` if the `Content-Type`
/// isn't `multipart/form-data`, the boundary is missing, or the body is
/// malformed. Parts without a `name=` in their `Content-Disposition` are
/// silently skipped (matches typical multipart-library leniency — a
/// malformed individual part shouldn't fail the whole upload).
pub async fn read_multipart_body(req: Request) -> Result<Vec<MultipartField>, Response> {
    let content_type = req
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    if !content_type.starts_with("multipart/form-data") {
        return Err(json_response(
            StatusCode::BAD_REQUEST,
            &serde_json::json!({ "error": "expected multipart/form-data request body" }),
        ));
    }
    let Some(boundary) = multipart_boundary(&content_type) else {
        return Err(json_response(
            StatusCode::BAD_REQUEST,
            &serde_json::json!({ "error": "multipart/form-data content-type missing boundary" }),
        ));
    };

    let bytes = match req.into_body().collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(_) => {
            return Err(json_response(
                StatusCode::BAD_REQUEST,
                &serde_json::json!({ "error": "failed to read request body" }),
            ))
        }
    };

    let delimiter = format!("--{boundary}").into_bytes();
    let mut fields = Vec::new();

    let Some(first_boundary) = find_subslice(&bytes, &delimiter, 0) else {
        return Err(json_response(
            StatusCode::BAD_REQUEST,
            &serde_json::json!({ "error": "malformed multipart body: boundary not found" }),
        ));
    };
    let mut pos = first_boundary + delimiter.len();

    loop {
        // A boundary immediately followed by "--" marks the terminal
        // boundary (RFC 7578 §4.1) -- stop parsing.
        if bytes[pos..].starts_with(b"--") {
            break;
        }
        if bytes[pos..].starts_with(b"\r\n") {
            pos += 2;
        }
        let Some(header_end) = find_subslice(&bytes, b"\r\n\r\n", pos) else {
            break;
        };
        let header_bytes = &bytes[pos..header_end];
        let headers_str = String::from_utf8_lossy(header_bytes);

        let mut name = None;
        let mut filename = None;
        let mut part_content_type = None;
        for line in headers_str.split("\r\n") {
            let lower = line.to_ascii_lowercase();
            if lower.starts_with("content-disposition:") {
                let (n, f) = parse_content_disposition(&line[line.find(':').map(|i| i + 1).unwrap_or(0)..]);
                name = n;
                filename = f;
            } else if lower.starts_with("content-type:") {
                part_content_type = line.split_once(':').map(|(_, v)| v.trim().to_string());
            }
        }

        let body_start = header_end + 4;
        let Some(next_boundary) = find_subslice(&bytes, &delimiter, body_start) else {
            break;
        };
        let mut body_end = next_boundary;
        if body_end >= body_start + 2 && &bytes[body_end - 2..body_end] == b"\r\n" {
            body_end -= 2;
        }
        let data = bytes[body_start..body_end].to_vec();

        if let Some(name) = name {
            fields.push(MultipartField { name, filename, content_type: part_content_type, data });
        }

        pos = next_boundary + delimiter.len();
    }

    Ok(fields)
}

/// `GET /health` and `GET /healthz` — poem-free equivalent of the handler
/// in `lib.rs`. Kept in lockstep with that JSON shape until the poem
/// version is retired.
///
/// Actually exercises the configured `DbBackend` (a real `list()` call, not
/// just "the process is up") so a load balancer sitting in front of
/// multiple instances (nginx/Caddy/Envoy — see `docs/deployment-scaling.md`)
/// can tell a genuinely-unhealthy instance (DB unreachable) apart from a
/// merely-busy one, and pull it out of rotation. A health check that always
/// answers 200 regardless of backend state defeats the point of clustering
/// behind a load balancer -- this used to be exactly that unconditional
/// check.
pub fn health_handler(state: Arc<crate::state::AppState>) -> Handler {
    #[derive(serde::Serialize)]
    struct Health {
        status: &'static str,
        service: &'static str,
        version: &'static str,
        backend: &'static str,
    }

    Arc::new(move |_req, _params| {
        let state = Arc::clone(&state);
        Box::pin(async move {
            // A dedicated, tiny table name that no real feature uses --
            // this is a connectivity probe, not a real query. `list()` is
            // implemented by every `DbBackend` and, for real (non-in-memory)
            // backends, actually round-trips to the store.
            match state.db.list("__health_check__").await {
                Ok(_) => json_response(
                    StatusCode::OK,
                    &Health {
                        status: "ok",
                        service: "open-runo-router",
                        version: env!("CARGO_PKG_VERSION"),
                        backend: state.db.backend_name(),
                    },
                ),
                Err(e) => {
                    tracing::error!(error = %e, backend = state.db.backend_name(), "health check: db backend unreachable");
                    json_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        &Health {
                            status: "degraded",
                            service: "open-runo-router",
                            version: env!("CARGO_PKG_VERSION"),
                            backend: state.db.backend_name(),
                        },
                    )
                }
            }
        })
    })
}

/// Serve a single static file from disk at request time, with a fixed
/// `content-type`. Used to host the WASM frontend bundle (`www/index.html`,
/// `www/pkg/*.js`, `www/pkg/*.wasm`) directly from `open-runo-router` —
/// no separate static-file server or Node.js tooling required.
pub fn static_file_handler(path: std::path::PathBuf, content_type: &'static str) -> Handler {
    Arc::new(move |_req, _params| {
        let path = path.clone();
        Box::pin(async move {
            match tokio::fs::read(&path).await {
                Ok(bytes) => HyperResponse::builder()
                    .status(StatusCode::OK)
                    .header("content-type", content_type)
                    .body(fixed_body(Bytes::from(bytes)))
                    .expect("building a response from a fixed set of valid headers cannot fail"),
                Err(_) => empty_status(StatusCode::NOT_FOUND),
            }
        })
    })
}

/// Serve `router` over a real TCP listener; returns the bound address and a
/// task handle. Used by tests (and, eventually, `main.rs`) to run the
/// poem-free stack end to end.
/// Resolves on Ctrl-C (`SIGINT`) or, on Unix, `SIGTERM` -- the two signals
/// that matter for "run N instances behind a reverse-proxy load balancer,
/// roll them one at a time": Ctrl-C for local/dev interrupts and `SIGTERM`
/// for how process supervisors (systemd, Docker, Kubernetes' `preStop` +
/// grace period, nginx/Caddy/Envoy-fronted rolling restarts) ask a process
/// to stop before escalating to `SIGKILL`. Intended to be passed as the
/// `shutdown` future to [`serve_with_shutdown`].
pub async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}

pub async fn serve(router: Router, addr: std::net::SocketAddr) -> std::io::Result<(std::net::SocketAddr, tokio::task::JoinHandle<()>)> {
    // No shutdown signal wired in -- runs forever until the process is
    // killed. Kept for callers (tests, quick examples) that don't care
    // about graceful shutdown. Production entrypoints should prefer
    // [`serve_with_shutdown`] so a rolling restart behind a load balancer
    // (nginx/Caddy/Envoy — see PORTING.md's horizontal-scaling recipe)
    // doesn't cut in-flight requests off mid-response.
    //
    // `std::future::pending()` never resolves, unlike an earlier version of
    // this wrapper that used a `watch::channel` sender scoped to this
    // function -- that sender got dropped the instant `serve` returned,
    // and a dropped `watch::Sender` makes the receiver's `changed()`
    // resolve immediately, so the "never fires" shutdown signal actually
    // fired right away and reset every connection moments after accepting
    // it.
    serve_with_shutdown(router, addr, std::future::pending())
        .await
        .map(|(addr, handle, _tracker)| (addr, handle))
}

/// Like [`serve`], but stops accepting new connections once `shutdown`
/// resolves, then waits for in-flight connections to finish their current
/// request/response (via hyper's `graceful_shutdown()`) before the returned
/// `JoinHandle` completes.
///
/// This is the piece Tomcat-behind-Apache HTTPD gets almost for free from
/// Apache's connection buffering: when Apache stops routing to a Tomcat
/// instance being taken out of rotation, in-flight requests already handed
/// to that Tomcat still get to finish. A tokio/hyper single-process server
/// has no such front door doing that for it, so if it needs to support
/// "run N instances behind a reverse-proxy load balancer, roll them one at
/// a time" (the deployment pattern this ecosystem's CLAUDE.md previously
/// dismissed as having "no equivalent need"), the equivalent behavior has
/// to be implemented here explicitly. See `docs/deployment-scaling.md` /
/// `PORTING.md` for the full reverse-proxy deployment recipe and the
/// research this is based on.
pub async fn serve_with_shutdown(
    router: Router,
    addr: std::net::SocketAddr,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<(std::net::SocketAddr, tokio::task::JoinHandle<()>, Arc<tokio::sync::Notify>)> {
    use hyper::server::conn::http1;
    use hyper_util::rt::TokioIo;
    use tokio::net::TcpListener;
    use tokio::sync::watch;
    use tokio::task::JoinSet;

    let listener = TcpListener::bind(addr).await?;
    let bound_addr = listener.local_addr()?;
    let router = Arc::new(router);

    // Broadcasts "start graceful shutdown" to every live connection task.
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    // Fires once every connection task spawned before shutdown has actually
    // finished draining -- unlike `handle` (which only proves the accept
    // loop stopped), this is what a test (or a caller doing its own
    // orchestration) should await to prove in-flight requests really
    // completed before the server considered itself shut down.
    let drained = Arc::new(tokio::sync::Notify::new());
    let drained_for_task = Arc::clone(&drained);

    let handle = tokio::spawn(async move {
        tokio::pin!(shutdown);
        let mut connections = JoinSet::new();
        loop {
            let accept_result = tokio::select! {
                biased;
                _ = &mut shutdown => break,
                res = listener.accept() => res,
            };
            let (stream, _) = match accept_result {
                Ok(pair) => pair,
                Err(_) => continue,
            };
            let io = TokioIo::new(stream);
            let router = Arc::clone(&router);
            let service = hyper::service::service_fn(move |req: Request| {
                let router = Arc::clone(&router);
                async move { Ok::<_, std::convert::Infallible>(router.dispatch(req).await) }
            });
            let mut conn_shutdown_rx = shutdown_rx.clone();
            connections.spawn(async move {
                // `.with_upgrades()` is required for `hyper::upgrade::on` to
                // ever resolve -- without it hyper tears the connection down
                // after the response instead of handing it off, and any
                // `websocket_handler` route would hang forever waiting for
                // an upgrade that never comes.
                let conn = http1::Builder::new()
                    .serve_connection(io, service)
                    .with_upgrades();
                let mut conn = std::pin::pin!(conn);
                loop {
                    tokio::select! {
                        res = conn.as_mut() => {
                            let _ = res;
                            break;
                        }
                        _ = conn_shutdown_rx.changed() => {
                            // Ask hyper to finish the current in-flight
                            // request/response and then close, instead of
                            // dropping the connection outright -- this is
                            // the actual "in-flight request completes
                            // before shutdown" guarantee.
                            conn.as_mut().graceful_shutdown();
                        }
                    }
                }
            });
        }
        // Stop accepting, then tell every already-live connection to wrap
        // up its current request/response instead of yanking it, and only
        // then declare the server drained.
        let _ = shutdown_tx.send(true);
        while connections.join_next().await.is_some() {}
        drained_for_task.notify_waiters();
    });

    Ok((bound_addr, handle, drained))
}

// ── TLS termination (rustls) ──────────────────────────────────────────────
//
// Poem-parity gap: Poem supports terminating TLS directly (native-tls /
// openssl / rustls backends) instead of always requiring a reverse proxy in
// front of it. This module is the rustls equivalent, gated behind the `tls`
// Cargo feature so deployments that keep terminating TLS at a reverse proxy
// (the existing default assumption for this app) don't pull in a TLS stack
// they never use. ACME (automatic certificate provisioning, e.g. Let's
// Encrypt) is a separate, much larger protocol -- see `docs/poem-parity.md`
// for why it's tracked as a distinct follow-up rather than bundled here.
#[cfg(feature = "tls")]
pub mod tls {
    use super::{Request, Response, Router};
    use std::io;
    use std::path::Path;
    use std::sync::Arc;
    use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
    use tokio_rustls::rustls::ServerConfig;
    use tokio_rustls::TlsAcceptor;

    /// Load a rustls [`ServerConfig`] from a PEM certificate chain and PEM
    /// private key on disk. No client-auth (mTLS) -- this app's auth is at
    /// the HTTP layer (`X-Api-Key` / session cookies), not the TLS layer.
    pub fn load_tls_config(cert_path: &Path, key_path: &Path) -> io::Result<ServerConfig> {
        let cert_file = std::fs::File::open(cert_path)?;
        let mut cert_reader = io::BufReader::new(cert_file);
        let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut cert_reader)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        if certs.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "no certificates found in cert file"));
        }

        let key_file = std::fs::File::open(key_path)?;
        let mut key_reader = io::BufReader::new(key_file);
        let key: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut key_reader)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no private key found in key file"))?;

        ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
    }

    /// Serve `router` over a real TLS-terminated TCP listener, using
    /// `tls_config` for the handshake. Otherwise identical to
    /// [`super::serve`] (same router dispatch, same `.with_upgrades()` for
    /// WebSocket support over TLS).
    pub async fn serve_tls(
        router: Router,
        addr: std::net::SocketAddr,
        tls_config: ServerConfig,
    ) -> io::Result<(std::net::SocketAddr, tokio::task::JoinHandle<()>)> {
        use hyper::server::conn::http1;
        use hyper_util::rt::TokioIo;
        use tokio::net::TcpListener;

        let listener = TcpListener::bind(addr).await?;
        let bound_addr = listener.local_addr()?;
        let router = Arc::new(router);
        let acceptor = TlsAcceptor::from(Arc::new(tls_config));

        let handle = tokio::spawn(async move {
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(pair) => pair,
                    Err(_) => continue,
                };
                let acceptor = acceptor.clone();
                let router = Arc::clone(&router);
                tokio::spawn(async move {
                    let tls_stream = match acceptor.accept(stream).await {
                        Ok(s) => s,
                        // Handshake failures (bad client, port scanner, TLS
                        // version mismatch) are routine on the open internet
                        // -- drop the connection, don't take the listener down.
                        Err(_) => return,
                    };
                    let io = TokioIo::new(tls_stream);
                    let service = hyper::service::service_fn(move |req: Request| {
                        let router = Arc::clone(&router);
                        async move { Ok::<Response, std::convert::Infallible>(router.dispatch(req).await) }
                    });
                    let _ = http1::Builder::new().serve_connection(io, service).with_upgrades().await;
                });
            }
        });

        Ok((bound_addr, handle))
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::hyper_compat::health_handler;
        use hyper::Method;

        /// Generates a throwaway self-signed cert/key pair (via `rcgen`,
        /// dev-dependency only) and writes them to a temp dir, so tests
        /// don't need a checked-in certificate. Follows the same
        /// `std::env::temp_dir()` + UUID pattern as
        /// `static_file_handler_serves_existing_file_and_404s_missing`.
        fn self_signed_cert_files() -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
            let dir = std::env::temp_dir().join(format!("orn-tls-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
                .expect("generate self-signed cert");
            let cert_path = dir.join("cert.pem");
            let key_path = dir.join("key.pem");
            std::fs::write(&cert_path, cert.cert.pem()).unwrap();
            std::fs::write(&key_path, cert.key_pair.serialize_pem()).unwrap();
            (cert_path, key_path, dir)
        }

        #[tokio::test]
        async fn health_endpoint_serves_over_real_tls() {
            let (cert_path, key_path, dir) = self_signed_cert_files();
            let tls_config = load_tls_config(&cert_path, &key_path).expect("load tls config");

            let router = Router::new().route(Method::GET, "/health", health_handler(Arc::new(crate::state::AppState::new())));
            let (addr, _handle) = serve_tls(router, "127.0.0.1:0".parse().unwrap(), tls_config)
                .await
                .expect("bind ephemeral TLS port");

            // Self-signed cert -> the client must be told not to verify the
            // chain against a real CA. This is a test-only relaxation
            // (`danger_accept_invalid_certs`); production deployments use a
            // CA-issued certificate and a normal client.
            let client = reqwest::Client::builder()
                .danger_accept_invalid_certs(true)
                .build()
                .expect("build TLS-tolerant client");

            let resp = client
                .get(format!("https://{addr}/health"))
                .send()
                .await
                .expect("TLS request should succeed");
            assert_eq!(resp.status(), reqwest::StatusCode::OK);
            let body: serde_json::Value = resp.json().await.expect("valid json body");
            assert_eq!(body["status"], "ok");

            let _ = std::fs::remove_dir_all(&dir);
        }

        #[tokio::test]
        async fn plain_http_client_cannot_talk_to_a_tls_listener() {
            let (cert_path, key_path, dir) = self_signed_cert_files();
            let tls_config = load_tls_config(&cert_path, &key_path).expect("load tls config");

            let router = Router::new().route(Method::GET, "/health", health_handler(Arc::new(crate::state::AppState::new())));
            let (addr, _handle) = serve_tls(router, "127.0.0.1:0".parse().unwrap(), tls_config)
                .await
                .expect("bind ephemeral TLS port");

            // A plain (non-TLS) client speaking HTTP to a TLS listener
            // should fail the request rather than somehow succeed --
            // proves the listener is actually enforcing a TLS handshake,
            // not just happening to work because the two ports overlap.
            let client = reqwest::Client::new();
            let result = client.get(format!("http://{addr}/health")).send().await;
            assert!(result.is_err(), "plain HTTP to a TLS-only listener should fail, not succeed");

            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn load_tls_config_errors_on_missing_files() {
            let result = load_tls_config(Path::new("/no/such/cert.pem"), Path::new("/no/such/key.pem"));
            assert!(result.is_err());
        }
    }
}

/// A single registered route: method + path pattern (`:name` segments) + handler.
struct Route {
    method: Method,
    segments: Vec<Segment>,
    handler: Handler,
}

#[derive(Clone, PartialEq)]
enum Segment {
    Literal(String),
    Param(String),
}

fn parse_pattern(pattern: &str) -> Vec<Segment> {
    pattern
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| {
            if let Some(name) = s.strip_prefix(':') {
                Segment::Param(name.to_string())
            } else {
                Segment::Literal(s.to_string())
            }
        })
        .collect()
}

/// Minimal method+path router. Not a general-purpose crate replacement —
/// just enough to dispatch open-runo-router's fixed endpoint set.
#[derive(Default)]
pub struct Router {
    routes: Vec<Route>,
}

impl Router {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn route(mut self, method: Method, pattern: &str, handler: Handler) -> Self {
        self.routes.push(Route {
            method,
            segments: parse_pattern(pattern),
            handler,
        });
        self
    }

    /// Register an `OPTIONS` route, wrapped in
    /// [`crate::middleware_hyper::with_cors`], for every distinct path
    /// pattern that has at least one other method registered but no
    /// `OPTIONS` handler of its own yet. Call this once, after all real
    /// routes are registered.
    ///
    /// Without this, [`Self::dispatch`] answers `OPTIONS` on any such
    /// path with a bare `405` -- generated by the router itself, before
    /// any handler (including CORS middleware) ever runs -- so a
    /// browser's CORS preflight (required whenever a cross-origin
    /// request sends a non-simple header like `X-Api-Key`, or most
    /// non-GET methods) can never succeed. This was a real, previously
    /// undetected bug: every route in production wired only its actual
    /// method (`GET`/`POST`/etc.), never `OPTIONS`, so cross-origin
    /// browser calls to almost the entire API were silently broken.
    /// Found via an actual cross-origin browser test, not just unit
    /// tests wrapping a single synthetic route in `with_cors` at both
    /// `GET` and `OPTIONS` (see CLAUDE.md HANDOFF, 2026-07-11).
    pub fn with_cors_preflight(mut self) -> Self {
        let candidates: Vec<Vec<Segment>> = self
            .routes
            .iter()
            .filter(|r| r.method != Method::OPTIONS)
            .map(|r| r.segments.clone())
            .collect();

        let mut added: Vec<Vec<Segment>> = Vec::new();
        for segments in candidates {
            let already_registered = self
                .routes
                .iter()
                .any(|r| r.method == Method::OPTIONS && r.segments == segments);
            let already_queued = added.iter().any(|s| *s == segments);
            if already_registered || already_queued {
                continue;
            }
            added.push(segments.clone());
            self.routes.push(Route {
                method: Method::OPTIONS,
                segments,
                // `with_cors` answers OPTIONS itself (see its `is_preflight`
                // branch) without ever calling the inner handler, so this
                // inner handler is never actually invoked.
                handler: crate::middleware_hyper::with_cors(Arc::new(|_req, _params| {
                    Box::pin(async { empty_status(StatusCode::OK) })
                })),
            });
        }
        self
    }

    fn match_path(&self, route: &Route, path: &str) -> Option<Params> {
        let path_segments: Vec<&str> = path.trim_matches('/').split('/').filter(|s| !s.is_empty()).collect();
        if path_segments.len() != route.segments.len() {
            return None;
        }
        let mut params = HashMap::new();
        for (seg, actual) in route.segments.iter().zip(path_segments.iter()) {
            match seg {
                Segment::Literal(lit) => {
                    if lit != actual {
                        return None;
                    }
                }
                Segment::Param(name) => {
                    params.insert(name.clone(), actual.to_string());
                }
            }
        }
        Some(Params(params))
    }

    pub fn dispatch(&self, req: Request) -> BoxFuture<Response> {
        let path = req.uri().path().to_string();
        let method = req.method().clone();

        for route in &self.routes {
            if route.method != method {
                continue;
            }
            if let Some(params) = self.match_path(route, &path) {
                let handler = Arc::clone(&route.handler);
                return handler(req, params);
            }
        }

        // Path matched by a different method → 405; otherwise 404.
        let path_exists = self
            .routes
            .iter()
            .any(|r| self.match_path(r, &path).is_some());
        let status = if path_exists {
            StatusCode::METHOD_NOT_ALLOWED
        } else {
            StatusCode::NOT_FOUND
        };
        Box::pin(async move { empty_status(status) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn h(status: StatusCode) -> Handler {
        Arc::new(move |_req, _params| Box::pin(async move { empty_status(status) }))
    }

    #[test]
    fn percent_decode_handles_plus_and_hex_escapes() {
        assert_eq!(percent_decode("hello+world"), "hello world");
        assert_eq!(percent_decode("a%2Fb%20c"), "a/b c");
        assert_eq!(percent_decode("plain"), "plain");
    }

    #[test]
    fn parses_literal_and_param_segments() {
        let segs = parse_pattern("/api/db/:table/:key");
        assert_eq!(segs.len(), 4);
        assert!(matches!(&segs[0], Segment::Literal(s) if s == "api"));
        assert!(matches!(&segs[1], Segment::Literal(s) if s == "db"));
        assert!(matches!(&segs[2], Segment::Param(s) if s == "table"));
        assert!(matches!(&segs[3], Segment::Param(s) if s == "key"));
    }

    #[tokio::test]
    async fn json_response_has_expected_content_type() {
        let resp = json_response(StatusCode::OK, &json!({ "ok": true }));
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "application/json"
        );
    }

    #[test]
    fn router_matches_literal_then_param_routes() {
        let router = Router::new()
            .route(Method::GET, "/health", h(StatusCode::OK))
            .route(Method::GET, "/api/db/:table/:key", h(StatusCode::IM_A_TEAPOT));

        let health = router.routes.iter().find(|r| r.method == Method::GET && matches!(r.segments.first(), Some(Segment::Literal(s)) if s == "health"));
        assert!(health.is_some());

        let dyn_route = &router.routes[1];
        let params = router.match_path(dyn_route, "/api/db/users/42").unwrap();
        assert_eq!(params.get("table"), Some("users"));
        assert_eq!(params.get("key"), Some("42"));
    }

    #[test]
    fn router_rejects_mismatched_segment_count() {
        let router = Router::new().route(Method::GET, "/api/db/:table/:key", h(StatusCode::OK));
        let route = &router.routes[0];
        assert!(router.match_path(route, "/api/db/users").is_none());
        assert!(router.match_path(route, "/api/db/users/42/extra").is_none());
    }

    #[tokio::test]
    async fn cors_preflight_reaches_a_real_handler_instead_of_a_bare_405() {
        // Before `with_cors_preflight`, an OPTIONS request to a path that
        // only registered GET/PUT/DELETE fell through Router::dispatch's
        // own 405 fallback -- generated by the router itself, before any
        // handler (including with_cors) ever ran. This reproduces exactly
        // that shape (multiple methods, no explicit OPTIONS route) and
        // confirms the auto-added preflight route now answers instead.
        let router = Router::new()
            .route(Method::GET, "/api/db/:table/:key", h(StatusCode::OK))
            .route(Method::PUT, "/api/db/:table/:key", h(StatusCode::OK))
            .route(Method::DELETE, "/api/db/:table/:key", h(StatusCode::OK))
            .with_cors_preflight();

        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .request(reqwest::Method::OPTIONS, format!("http://{addr}/api/db/users/42"))
            .header("origin", "https://example.com")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        assert!(resp.headers().contains_key("access-control-allow-origin"));
    }

    #[test]
    fn with_cors_preflight_does_not_duplicate_an_explicit_options_route() {
        let router = Router::new()
            .route(Method::GET, "/x", h(StatusCode::OK))
            .route(Method::OPTIONS, "/x", h(StatusCode::IM_A_TEAPOT))
            .with_cors_preflight();

        let options_routes: Vec<_> = router.routes.iter().filter(|r| r.method == Method::OPTIONS).collect();
        assert_eq!(options_routes.len(), 1, "should not add a second OPTIONS route for the same path");
    }

    #[test]
    fn with_cors_preflight_adds_exactly_one_options_route_per_distinct_path() {
        // /api/db/:table/:key is registered under 3 methods; it should
        // still only get ONE auto-added OPTIONS route, not three.
        let router = Router::new()
            .route(Method::GET, "/api/db/:table/:key", h(StatusCode::OK))
            .route(Method::PUT, "/api/db/:table/:key", h(StatusCode::OK))
            .route(Method::DELETE, "/api/db/:table/:key", h(StatusCode::OK))
            .with_cors_preflight();

        let options_routes: Vec<_> = router.routes.iter().filter(|r| r.method == Method::OPTIONS).collect();
        assert_eq!(options_routes.len(), 1);
    }

    /// End-to-end: real TCP listener, real hyper connection, real HTTP
    /// client — proves the poem-free stack actually serves traffic, not
    /// just that in-process function calls type-check.
    #[tokio::test]
    async fn health_endpoint_serves_over_real_http() {
        let router = Router::new()
            .route(Method::GET, "/health", health_handler(Arc::new(crate::state::AppState::new())))
            .route(Method::GET, "/healthz", health_handler(Arc::new(crate::state::AppState::new())));

        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let client = reqwest::Client::new();

        let resp = client
            .get(format!("http://{addr}/health"))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert_eq!(body["status"], "ok");
        assert_eq!(body["service"], "open-runo-router");

        let resp = client
            .get(format!("http://{addr}/healthz"))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);

        let resp = client
            .get(format!("http://{addr}/nonexistent"))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
    }

    /// End-to-end: a real multipart/form-data body (constructed by hand,
    /// with the exact boundary framing browsers send), routed through a
    /// handler that calls `read_multipart_body` over real HTTP.
    #[tokio::test]
    async fn multipart_body_parses_text_and_file_fields_over_real_http() {
        let router = Router::new().route(
            Method::POST,
            "/upload",
            Arc::new(|req, _params| {
                Box::pin(async move {
                    match read_multipart_body(req).await {
                        Ok(fields) => {
                            let summary: Vec<_> = fields
                                .iter()
                                .map(|f| {
                                    serde_json::json!({
                                        "name": f.name,
                                        "filename": f.filename,
                                        "content_type": f.content_type,
                                        "data": String::from_utf8_lossy(&f.data),
                                    })
                                })
                                .collect();
                            json_response(StatusCode::OK, &summary)
                        }
                        Err(resp) => resp,
                    }
                })
            }),
        );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let form = reqwest::multipart::Form::new()
            .text("service_name", "users")
            .text("stage", "local")
            .part(
                "sdl_file",
                reqwest::multipart::Part::bytes(b"type User { id: ID! }".to_vec())
                    .file_name("users.graphql")
                    .mime_str("text/plain")
                    .unwrap(),
            );

        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/upload"))
            .multipart(form)
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);

        let body: serde_json::Value = resp.json().await.expect("valid json body");
        let fields = body.as_array().expect("array of fields");
        assert_eq!(fields.len(), 3);

        let by_name = |name: &str| fields.iter().find(|f| f["name"] == name).expect("field present");
        assert_eq!(by_name("service_name")["data"], "users");
        assert_eq!(by_name("stage")["data"], "local");
        let file_field = by_name("sdl_file");
        assert_eq!(file_field["filename"], "users.graphql");
        assert_eq!(file_field["data"], "type User { id: ID! }");
    }

    #[test]
    fn multipart_boundary_extracts_from_content_type() {
        assert_eq!(
            multipart_boundary("multipart/form-data; boundary=abc123"),
            Some("abc123".to_string())
        );
        assert_eq!(
            multipart_boundary("multipart/form-data; boundary=\"quoted-boundary\""),
            Some("quoted-boundary".to_string())
        );
        assert_eq!(multipart_boundary("application/json"), None);
    }

    #[tokio::test]
    async fn read_multipart_body_rejects_non_multipart_content_type() {
        let router = Router::new().route(
            Method::POST,
            "/upload",
            Arc::new(|req, _params| {
                Box::pin(async move {
                    match read_multipart_body(req).await {
                        Ok(_) => empty_status(StatusCode::OK),
                        Err(resp) => resp,
                    }
                })
            }),
        );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/upload"))
            .json(&serde_json::json!({ "not": "multipart" }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn static_file_handler_serves_existing_file_and_404s_missing() {
        let dir = std::env::temp_dir().join(format!("orn-static-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file_path = dir.join("hello.txt");
        std::fs::write(&file_path, b"hello static world").unwrap();

        let router = Router::new()
            .route(Method::GET, "/hello.txt", static_file_handler(file_path.clone(), "text/plain"))
            .route(Method::GET, "/missing.txt", static_file_handler(dir.join("missing.txt"), "text/plain"));
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");
        let client = reqwest::Client::new();

        let resp = client
            .get(format!("http://{addr}/hello.txt"))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        assert_eq!(resp.headers().get("content-type").unwrap(), "text/plain");
        assert_eq!(resp.text().await.unwrap(), "hello static world");

        let resp = client
            .get(format!("http://{addr}/missing.txt"))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn graceful_shutdown_lets_an_in_flight_request_finish_before_the_server_stops() {
        // Reproduces the exact guarantee the Tomcat-behind-Apache-HTTPD
        // analogy raised (see CLAUDE.md / docs/deployment-scaling.md):
        // when a load balancer takes an instance out of rotation and asks
        // it to shut down, a request already being handled must still get
        // its response, not be severed mid-flight. Uses a slow handler
        // (sleeps past the moment shutdown is triggered) to prove this is
        // a real race being won, not just "shutdown never got a chance to
        // interrupt anything."
        use tokio::sync::oneshot;

        let router = Router::new().route(
            Method::GET,
            "/slow",
            Arc::new(move |_req, _params| {
                Box::pin(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    empty_status(StatusCode::OK)
                })
            }),
        );

        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let (addr, handle, _drained) = serve_with_shutdown(router, "127.0.0.1:0".parse().unwrap(), async move {
            let _ = shutdown_rx.await;
        })
        .await
        .expect("bind ephemeral port");

        let client = reqwest::Client::new();
        // `reqwest`'s `.send()` future is lazy like any other Rust future --
        // it does not open the connection until polled. Binding it to a
        // local without spawning/polling it does nothing until the final
        // `.await`, which would happen *after* shutdown already fired below,
        // defeating the whole point of this test (the request would never
        // even have connected while "in flight"). Spawning it is what
        // actually starts the request now.
        let spawn_client = client.clone();
        let request = tokio::spawn(async move { spawn_client.get(format!("http://{addr}/slow")).send().await });

        // Give the request time to be accepted and reach the 200ms sleep,
        // then trigger shutdown while it's still in flight.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        shutdown_tx.send(()).expect("shutdown receiver still alive");

        // The in-flight request must still complete successfully...
        let resp = request
            .await
            .expect("request task should not panic")
            .expect("in-flight request should still complete, not be severed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);

        // ...and the server task itself must terminate (proving it did not
        // just ignore the shutdown signal) once that connection drains.
        tokio::time::timeout(std::time::Duration::from_secs(5), handle)
            .await
            .expect("server should shut down promptly after draining")
            .expect("server task should not panic");

        // A brand-new connection attempt after shutdown must fail --
        // proving the listener actually stopped accepting, not just that
        // the one in-flight request happened to finish on its own.
        let post_shutdown = client.get(format!("http://{addr}/slow")).send().await;
        assert!(post_shutdown.is_err(), "listener should have stopped accepting new connections");
    }
}
