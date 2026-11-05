//! gRPC-web-Connect のサーバー部。
//!
//! [`Service`]を実装して[`handle`]に渡すだけで、同じパス(`/<パッケージ.サービス>/<メソッド>`)で
//! **gRPC / gRPC-web / gRPC-web-text / Connect(proto・JSON、単項・サーバーストリーミング)**を
//! Content-Typeで判別して処理する。処理本体(`Service::call`)は通信方式を知らない=二重実装にならない。
//! 認証に使う鍵などは`HeaderMap`(gRPCメタデータ=HTTPヘッダ)で受け取る。
//!
//! ハイブリッドの要点:
//! - Connectのエラーは**HTTPステータス+JSON**で返しつつ、gRPC由来のツールが読めるよう
//!   `grpc-status`/`grpc-message`ヘッダも付ける。
//! - gRPC-webのエラーは200+末尾のトレーラーフレーム、gRPCはHTTP/2トレーラー。
//! - ストリームの終端は、gRPC=トレーラー、gRPC-web=`0x80`フレーム、Connect=`0x02`終端フレーム。

use async_trait::async_trait;
use bytes::Bytes;
use futures::{Stream, StreamExt};
use open_runo_connect_core as core;
use open_runo_connect_core::{code, Codec, Protocol, Status};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Limited, StreamBody};
use hyper::body::{Body as HttpBody, Frame};
use hyper::{HeaderMap, Response, StatusCode};
use std::convert::Infallible;
use std::pin::Pin;

pub use open_runo_connect_core as proto_core;
#[cfg(feature = "json")]
pub use prost_reflect;

/// 要求1件あたりの最大サイズ(これを超えるとRESOURCE_EXHAUSTED)。
pub const MAX_REQUEST_BYTES: usize = 4 * 1024 * 1024;

/// 応答のボディ型(固定でもストリームでもこれ)。
pub type Body = BoxBody<Bytes, Infallible>;

/// サーバーストリーミングの応答(各要素は1メッセージのprotoバイト列)。
pub type MessageStream = Pin<Box<dyn Stream<Item = Vec<u8>> + Send + Sync>>;

/// 応答本体。
pub enum Reply {
    Unary(Vec<u8>),
    Stream(MessageStream),
}

/// 呼び出し結果(本体 + 応答ヘッダ。`set-cookie`など)。
pub struct Output {
    pub reply: Reply,
    pub headers: Vec<(String, String)>,
}

impl Output {
    pub fn unary(bytes: Vec<u8>) -> Self {
        Self {
            reply: Reply::Unary(bytes),
            headers: Vec::new(),
        }
    }
    pub fn stream(s: MessageStream) -> Self {
        Self {
            reply: Reply::Stream(s),
            headers: Vec::new(),
        }
    }
}

/// メソッドの定義(ストリームか、JSON変換用の記述子)。
#[derive(Clone)]
pub struct MethodInfo {
    pub server_streaming: bool,
    /// Connect/gRPC-webのJSONコーデック用(`json` feature)。無ければJSONは`unimplemented`。
    #[cfg(feature = "json")]
    pub descriptors: Option<(
        prost_reflect::MessageDescriptor,
        prost_reflect::MessageDescriptor,
    )>,
}

impl MethodInfo {
    pub fn unary() -> Self {
        Self {
            server_streaming: false,
            #[cfg(feature = "json")]
            descriptors: None,
        }
    }
    pub fn server_streaming() -> Self {
        Self {
            server_streaming: true,
            #[cfg(feature = "json")]
            descriptors: None,
        }
    }
    /// 記述子(入力・出力メッセージ)つき。JSONコーデックが使える。
    #[cfg(feature = "json")]
    pub fn with_descriptors(
        server_streaming: bool,
        input: prost_reflect::MessageDescriptor,
        output: prost_reflect::MessageDescriptor,
    ) -> Self {
        Self {
            server_streaming,
            descriptors: Some((input, output)),
        }
    }
}

/// 提供するサービス。通信方式に依存しない処理本体。
#[async_trait]
pub trait Service: Send + Sync + 'static {
    /// 完全修飾名(例: `open_runo.admin.v1.Admin`)。パスは`/<これ>/<メソッド>`。
    fn service_name(&self) -> &str;
    /// メソッドの定義。知らないメソッドは`None`(→UNIMPLEMENTED)。
    fn method(&self, name: &str) -> Option<MethodInfo>;
    /// 1回の呼び出し。`message`はprotoバイト列(JSONは変換済み)。認証・権限・監査はここで行う。
    async fn call(
        &self,
        method: &str,
        headers: &HeaderMap,
        message: &[u8],
    ) -> Result<Output, Status>;
}

/// `/<service>/<method>`から`method`を取り出す。
pub fn method_of<'a>(service_name: &str, path: &'a str) -> Option<&'a str> {
    path.strip_prefix('/')?
        .strip_prefix(service_name)?
        .strip_prefix('/')
        .filter(|m| !m.is_empty() && !m.contains('/'))
}

// ── 応答の組み立て ───────────────────────────────────────────────────────

fn builder(
    protocol: Protocol,
    status: u16,
    extra: &[(String, String)],
) -> hyper::http::response::Builder {
    let mut b = Response::builder()
        .status(status)
        .header("content-type", protocol.response_content_type());
    for (k, v) in extra {
        b = b.header(k.as_str(), v.as_str());
    }
    b
}

fn stream_body<S>(frames: S) -> Body
where
    S: Stream<Item = Frame<Bytes>> + Send + Sync + 'static,
{
    BodyExt::boxed(StreamBody::new(frames.map(Ok::<_, Infallible>)))
}

/// 終端(状態)のフレーム。gRPC=トレーラー、gRPC-web=`0x80`フレーム、gRPC-web-text=そのbase64、
/// Connectストリーム=`0x02`終端フレーム。
fn finish(protocol: Protocol, st: &Status) -> Frame<Bytes> {
    match protocol {
        Protocol::Grpc => {
            let mut t = HeaderMap::new();
            t.insert("grpc-status", st.code.to_string().parse().expect("digits"));
            if let Ok(v) = core::percent_encode(&st.message).parse() {
                t.insert("grpc-message", v);
            }
            Frame::trailers(t)
        }
        Protocol::GrpcWeb => {
            Frame::data(Bytes::from(core::encode_web_trailers(st.code, &st.message)))
        }
        Protocol::GrpcWebText => Frame::data(Bytes::from(core::b64_encode(
            &core::encode_web_trailers(st.code, &st.message),
        ))),
        Protocol::ConnectStream(_) | Protocol::ConnectUnary(_) => Frame::data(Bytes::from(
            core::encode_connect_end((st.code != code::OK).then_some(st)),
        )),
    }
}

/// データフレーム(1メッセージ)を通信方式の形にする。
fn data_frame(protocol: Protocol, message: &[u8]) -> Frame<Bytes> {
    let framed = core::encode_frame(message);
    match protocol {
        Protocol::GrpcWebText => Frame::data(Bytes::from(core::b64_encode(&framed))),
        _ => Frame::data(Bytes::from(framed)),
    }
}

fn fixed(status: u16, content_type: &str, body: Vec<u8>) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("content-type", content_type)
        .body(BodyExt::boxed(http_body_util::Full::new(Bytes::from(body))))
        .expect("fixed headers are valid")
}

/// Connect単項のエラー応答: **HTTPステータス+JSON**、さらにgRPC由来のツール向けに
/// `grpc-status`/`grpc-message`ヘッダも付ける(ハイブリッド)。
fn connect_error(st: &Status) -> Response<Body> {
    let mut resp = fixed(
        code::http_status(st.code),
        "application/json",
        serde_json::to_vec(&st.connect_json()).unwrap_or_default(),
    );
    resp.headers_mut()
        .insert("grpc-status", st.code.to_string().parse().expect("digits"));
    if let Ok(v) = core::percent_encode(&st.message).parse() {
        resp.headers_mut().insert("grpc-message", v);
    }
    resp
}

/// 方式に合わせたエラー応答(本文なし)。
fn error_response(protocol: Option<Protocol>, st: &Status) -> Response<Body> {
    match protocol {
        None | Some(Protocol::ConnectUnary(_)) => connect_error(st),
        Some(
            p @ (Protocol::ConnectStream(_)
            | Protocol::Grpc
            | Protocol::GrpcWeb
            | Protocol::GrpcWebText),
        ) => {
            // ストリーミング/gRPC系は、エラーも「終端フレーム」で返す(HTTPは200)。
            builder(p, 200, &[])
                .body(stream_body(futures::stream::iter([finish(p, st)])))
                .expect("fixed headers are valid")
        }
    }
}

// ── JSON⇔proto ───────────────────────────────────────────────────────────

#[cfg(feature = "json")]
fn json_to_proto(info: &MethodInfo, body: &[u8]) -> Result<Vec<u8>, Status> {
    use prost_reflect::prost::Message as _;
    use prost_reflect::DynamicMessage;
    let Some((input, _)) = &info.descriptors else {
        return Err(Status::new(
            code::UNIMPLEMENTED,
            "JSON codec needs method descriptors",
        ));
    };
    let text: &[u8] = if body.is_empty() { b"{}" } else { body };
    let mut de = serde_json::Deserializer::from_slice(text);
    let msg = DynamicMessage::deserialize(input.clone(), &mut de)
        .map_err(|e| Status::new(code::INVALID_ARGUMENT, format!("bad JSON request: {e}")))?;
    de.end()
        .map_err(|e| Status::new(code::INVALID_ARGUMENT, format!("bad JSON request: {e}")))?;
    Ok(msg.encode_to_vec())
}

#[cfg(feature = "json")]
fn proto_to_json(info: &MethodInfo, bytes: &[u8]) -> Result<Vec<u8>, Status> {
    use prost_reflect::{DynamicMessage, SerializeOptions};
    let Some((_, output)) = &info.descriptors else {
        return Err(Status::new(
            code::UNIMPLEMENTED,
            "JSON codec needs method descriptors",
        ));
    };
    let msg = DynamicMessage::decode(output.clone(), bytes)
        .map_err(|e| Status::new(code::INTERNAL, format!("bad response message: {e}")))?;
    let mut out = Vec::new();
    let mut ser = serde_json::Serializer::new(&mut out);
    msg.serialize_with_options(&mut ser, &SerializeOptions::new())
        .map_err(|e| Status::new(code::INTERNAL, format!("cannot encode JSON: {e}")))?;
    Ok(out)
}

#[cfg(not(feature = "json"))]
fn json_to_proto(_: &MethodInfo, _: &[u8]) -> Result<Vec<u8>, Status> {
    Err(Status::new(
        code::UNIMPLEMENTED,
        "JSON codec is not enabled",
    ))
}

#[cfg(not(feature = "json"))]
fn proto_to_json(_: &MethodInfo, _: &[u8]) -> Result<Vec<u8>, Status> {
    Err(Status::new(
        code::UNIMPLEMENTED,
        "JSON codec is not enabled",
    ))
}

fn decode_body(codec: Codec, info: &MethodInfo, body: &[u8]) -> Result<Vec<u8>, Status> {
    match codec {
        Codec::Proto => Ok(body.to_vec()),
        Codec::Json => json_to_proto(info, body),
    }
}

fn encode_body(codec: Codec, info: &MethodInfo, bytes: &[u8]) -> Result<Vec<u8>, Status> {
    match codec {
        Codec::Proto => Ok(bytes.to_vec()),
        Codec::Json => proto_to_json(info, bytes),
    }
}

// ── 本体 ─────────────────────────────────────────────────────────────────

/// 1要求を処理する。`B`はhyperの要求本文(`Incoming`など)。
pub async fn handle<S, B>(service: &S, req: hyper::Request<B>) -> Response<Body>
where
    S: Service + ?Sized,
    B: HttpBody + Send + 'static,
    B::Data: Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let (parts, body) = req.into_parts();
    let ct = parts
        .headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let Some(protocol) = Protocol::from_content_type(&ct) else {
        return error_response(
            None,
            &Status::new(code::INVALID_ARGUMENT, "unsupported content-type"),
        );
    };
    let Some(method) = method_of(service.service_name(), parts.uri.path()).map(str::to_string)
    else {
        return error_response(
            Some(protocol),
            &Status::new(code::UNIMPLEMENTED, "unknown service"),
        );
    };
    if parts.method != hyper::Method::POST {
        return error_response(
            Some(protocol),
            &Status::new(code::UNIMPLEMENTED, "only POST is supported"),
        );
    }
    let Some(info) = service.method(&method) else {
        return error_response(
            Some(protocol),
            &Status::new(code::UNIMPLEMENTED, format!("unknown method: {method}")),
        );
    };
    // Connectは単項=application/proto|json、ストリーミング=application/connect+*で呼ぶ決まり。
    match protocol {
        Protocol::ConnectUnary(_) if info.server_streaming => {
            // 呼び方の取り違えはプロトコル違反: ストリームが始まる前なのでHTTPステータスで返す。
            return connect_error(&Status::new(
                code::INVALID_ARGUMENT,
                "this method streams: use application/connect+proto or application/connect+json",
            ));
        }
        Protocol::ConnectStream(_) if !info.server_streaming => {
            return connect_error(&Status::new(
                code::INVALID_ARGUMENT,
                "this method is unary: use application/proto or application/json",
            ));
        }
        _ => {}
    }
    // 圧縮は未対応(identityのみ)。
    let enc_header = if matches!(protocol, Protocol::ConnectStream(_)) {
        "connect-content-encoding"
    } else {
        "content-encoding"
    };
    if let Some(enc) = parts.headers.get(enc_header).and_then(|v| v.to_str().ok()) {
        if !enc.eq_ignore_ascii_case("identity") {
            return error_response(
                Some(protocol),
                &Status::new(code::UNIMPLEMENTED, "compression is not supported"),
            );
        }
    }

    let collected = match Limited::new(body, MAX_REQUEST_BYTES + 5).collect().await {
        Ok(c) => c.to_bytes(),
        Err(_) => {
            return error_response(
                Some(protocol),
                &Status::new(code::RESOURCE_EXHAUSTED, "request too large"),
            );
        }
    };
    let collected = if protocol == Protocol::GrpcWebText {
        match core::decode_text_body(&collected) {
            Some(b) => Bytes::from(b),
            None => {
                return error_response(
                    Some(protocol),
                    &Status::new(code::INVALID_ARGUMENT, "invalid base64 body"),
                );
            }
        }
    } else {
        collected
    };

    // 本文 → protoバイト列。
    let enveloped = !matches!(protocol, Protocol::ConnectUnary(_));
    let raw: Vec<u8> = if enveloped {
        match core::split_frame(&collected) {
            Some((0, b, _)) => b.to_vec(),
            Some(_) => {
                return error_response(
                    Some(protocol),
                    &Status::new(code::UNIMPLEMENTED, "compressed messages are not supported"),
                );
            }
            None if collected.is_empty() => Vec::new(),
            None => {
                return error_response(
                    Some(protocol),
                    &Status::new(code::INVALID_ARGUMENT, "malformed frame"),
                );
            }
        }
    } else {
        collected.to_vec()
    };
    let codec = match protocol {
        Protocol::ConnectUnary(c) | Protocol::ConnectStream(c) => c,
        _ => Codec::Proto,
    };
    let message = match decode_body(codec, &info, &raw) {
        Ok(m) => m,
        Err(e) => return error_response(Some(protocol), &e),
    };

    match service.call(&method, &parts.headers, &message).await {
        Err(e) => error_response(Some(protocol), &e),
        Ok(Output {
            reply: Reply::Unary(bytes),
            headers,
        }) => match protocol {
            Protocol::ConnectUnary(c) => match encode_body(c, &info, &bytes) {
                Ok(b) => {
                    let mut resp = fixed(200, protocol.response_content_type(), b);
                    for (k, v) in headers {
                        if let (Ok(k), Ok(v)) = (
                            hyper::header::HeaderName::from_bytes(k.as_bytes()),
                            hyper::header::HeaderValue::from_str(&v),
                        ) {
                            resp.headers_mut().append(k, v);
                        }
                    }
                    resp
                }
                Err(e) => error_response(Some(protocol), &e),
            },
            _ => {
                let payload = match encode_body(codec, &info, &bytes) {
                    Ok(p) => p,
                    Err(e) => return error_response(Some(protocol), &e),
                };
                let frames = [
                    data_frame(protocol, &payload),
                    finish(protocol, &Status::new(code::OK, "")),
                ];
                builder(protocol, 200, &headers)
                    .body(stream_body(futures::stream::iter(frames)))
                    .expect("fixed headers are valid")
            }
        },
        Ok(Output {
            reply: Reply::Stream(s),
            headers,
        }) => {
            let info2 = info.clone();
            let data = s.filter_map(move |m| {
                let r = encode_body(codec, &info2, &m)
                    .ok()
                    .map(|b| data_frame(protocol, &b));
                futures::future::ready(r)
            });
            let end =
                futures::stream::once(async move { finish(protocol, &Status::new(code::OK, "")) });
            builder(protocol, 200, &headers)
                .body(stream_body(data.chain(end)))
                .expect("fixed headers are valid")
        }
    }
}

/// HTTPステータスだけ返したいとき用(OPTIONSなど)。
pub fn empty(status: StatusCode) -> Response<Body> {
    fixed(status.as_u16(), "text/plain", Vec::new())
}

#[cfg(test)]
mod tests;
