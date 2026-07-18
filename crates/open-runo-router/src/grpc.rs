//! gRPC — Poem-parity gap ("gRPC(poem-grpc相当)", `docs/poem-parity.md`).
//!
//! A minimal, real gRPC-over-HTTP/2 server: not a general-purpose gRPC
//! framework (no code generation from `.proto` files, no arbitrary
//! service registration), but a working implementation of two real,
//! well-known services --
//! [`grpc.health.v1.Health`](https://github.com/grpc/grpc/blob/master/doc/health-checking.md)
//! (both its unary `Check` and server-streaming `Watch` RPCs, the latter
//! added 2026-07-12 to close this module's earlier "no streaming" gap)
//! and a minimal
//! [`grpc.reflection.v1.ServerReflection`](https://github.com/grpc/grpc/blob/master/doc/server-reflection.md)
//! (added 2026-07-12: `list_services` for service discovery, e.g.
//! `grpcurl <addr> list`; extended 2026-07-14 with `file_containing_symbol`
//! for schema introspection, e.g. `grpcurl <addr> describe
//! grpc.health.v1.Health`, and 2026-07-18 with `file_by_filename`, e.g.
//! `grpcurl <addr> descriptor grpc/health/v1/health.proto`, both backed by
//! the same hand-rolled `FileDescriptorProto` for
//! `grpc/health/v1/health.proto`) -- proving the
//! transport (HTTP/2 framing, the gRPC length-prefixed message envelope,
//! trailers-based `grpc-status`) and the wire format (Protocol Buffers)
//! both work end to end for unary, server-streaming, and
//! embedded-message-encoding RPC shapes.
//!
//! **No new dependencies.** HTTP/2 comes from `hyper`'s existing `full`
//! feature (already pulls in the `h2` crate transitively; this module is
//! the first thing in the crate to actually construct an
//! `hyper::server::conn::http2::Builder`, but the dependency itself was
//! already present). Protobuf encoding/decoding is hand-rolled for the
//! two tiny messages this module actually needs
//! (`HealthCheckRequest`/`HealthCheckResponse`) rather than pulling in a
//! full protobuf codegen pipeline (`prost`/`tonic`) -- the wire format for
//! these two messages is a handful of varints and one length-delimited
//! string, well within the "hand-roll the data shape" precedent this
//! crate already sets for multipart and WebSocket framing.
//!
//! **Why a separate port, not the same listener as the REST API**: HTTP/2
//! without TLS ("h2c") normally requires either an `Upgrade: h2c` header
//! on an HTTP/1.1 request or "prior knowledge" (the client sends the
//! HTTP/2 connection preface immediately). Distinguishing an incoming
//! plain-TCP connection's protocol before the first bytes arrive would
//! require protocol sniffing on every connection to the main REST
//! listener, adding complexity and latency there for a feature most
//! deployments won't use. A dedicated port (the same pattern
//! `hyper_compat::tls::serve_tls` uses for TLS) keeps the common path
//! simple; gRPC-aware clients connect straight to it with prior knowledge.

use bytes::{Bytes, BytesMut};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::{HeaderMap, Request as HyperRequest, Response as HyperResponse, StatusCode};
use std::convert::Infallible;

type Request = HyperRequest<Incoming>;
type Response = HyperResponse<BoxBody<Bytes, Infallible>>;

/// gRPC status codes actually used here (a small subset of the full
/// [status code table](https://grpc.io/docs/guides/status-codes/)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrpcStatus {
    Ok = 0,
    Unknown = 2,
    InvalidArgument = 3,
    NotFound = 5,
    Unimplemented = 12,
    Internal = 13,
}

// ── gRPC message framing (not protobuf itself -- the length-prefix
// envelope every gRPC message body is wrapped in, per
// https://github.com/grpc/grpc/blob/master/doc/PROTOCOL-HTTP2.md) ────────

/// Strip the 5-byte gRPC frame header (1 compression-flag byte + 4
/// big-endian length bytes) off `body` and return the inner message
/// bytes. Rejects a compressed frame (flag byte != 0) -- this
/// implementation never sends `grpc-encoding`, so a compressed request
/// would mean a client assuming compression was negotiated when it
/// wasn't.
fn decode_grpc_frame(body: &[u8]) -> Result<&[u8], GrpcStatus> {
    if body.len() < 5 {
        return Err(GrpcStatus::InvalidArgument);
    }
    let compressed = body[0] != 0;
    if compressed {
        return Err(GrpcStatus::Unimplemented);
    }
    let len = u32::from_be_bytes([body[1], body[2], body[3], body[4]]) as usize;
    let message = body.get(5..5 + len).ok_or(GrpcStatus::InvalidArgument)?;
    Ok(message)
}

/// Wrap `message` in a gRPC frame (uncompressed).
fn encode_grpc_frame(message: &[u8]) -> Bytes {
    let mut out = BytesMut::with_capacity(5 + message.len());
    out.extend_from_slice(&[0u8]); // uncompressed
    out.extend_from_slice(&(message.len() as u32).to_be_bytes());
    out.extend_from_slice(message);
    out.freeze()
}

// ── Minimal hand-rolled Protocol Buffers codec ───────────────────────────
// Just enough of the wire format (varints, length-delimited fields) for
// the two messages this module needs. Not a general-purpose protobuf
// implementation.

fn encode_varint(mut value: u64, out: &mut BytesMut) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.extend_from_slice(&[byte]);
            break;
        }
        out.extend_from_slice(&[byte | 0x80]);
    }
}

fn decode_varint(buf: &[u8]) -> Option<(u64, &[u8])> {
    let mut value: u64 = 0;
    for (i, &byte) in buf.iter().enumerate().take(10) {
        value |= ((byte & 0x7f) as u64) << (7 * i);
        if byte & 0x80 == 0 {
            return Some((value, &buf[i + 1..]));
        }
    }
    None
}

/// Encode a `string` field (wire type 2, length-delimited).
fn encode_string_field(field_number: u32, value: &str) -> BytesMut {
    let mut out = BytesMut::new();
    encode_varint(((field_number as u64) << 3) | 2, &mut out);
    encode_varint(value.len() as u64, &mut out);
    out.extend_from_slice(value.as_bytes());
    out
}

/// Encode an embedded (nested) message field (wire type 2,
/// length-delimited, same framing as a `string`/`bytes` field -- protobuf
/// doesn't distinguish them at the wire level, only in the generated
/// code's type).
fn encode_embedded_message(field_number: u32, submessage: &[u8]) -> BytesMut {
    let mut out = BytesMut::new();
    encode_varint(((field_number as u64) << 3) | 2, &mut out);
    encode_varint(submessage.len() as u64, &mut out);
    out.extend_from_slice(submessage);
    out
}

/// Scan a `ServerReflectionRequest` for which `message_request` oneof
/// field (3-6) was set, without needing every possible request shape --
/// this reflection implementation only understands `list_services`
/// (field 7); returns `true` iff field 7 is present with wire type 2.
/// Deliberately tolerant of the request's exact string contents (the
/// spec allows `list_services` to be an arbitrary/empty string -- some
/// clients send the target host, others send nothing).
fn request_is_list_services(bytes: &[u8]) -> bool {
    let mut buf = bytes;
    while !buf.is_empty() {
        let Some((tag, rest)) = decode_varint(buf) else {
            return false;
        };
        let field_number = tag >> 3;
        let wire_type = tag & 0x7;
        buf = rest;
        match wire_type {
            2 => {
                let Some((len, rest)) = decode_varint(buf) else {
                    return false;
                };
                let len = len as usize;
                if rest.len() < len {
                    return false;
                }
                if field_number == 7 {
                    return true;
                }
                buf = &rest[len..];
            }
            0 => {
                let Some((_, rest)) = decode_varint(buf) else {
                    return false;
                };
                buf = rest;
            }
            _ => return false,
        }
    }
    false
}

/// Build the `ServerReflectionResponse` bytes for a successful
/// `list_services` request: field 6 (`list_services_response`) wraps a
/// `ListServiceResponse { repeated ServiceResponse service = 1; }`, and
/// each `ServiceResponse` is just `{ string name = 1; }`.
fn encode_list_services_response(service_names: &[&str]) -> Bytes {
    let mut list_response = BytesMut::new();
    for name in service_names {
        let service_response = encode_string_field(1, name);
        list_response.extend_from_slice(&encode_embedded_message(1, &service_response));
    }
    let response = encode_embedded_message(6, &list_response);
    response.freeze()
}

/// Encode a `bytes` field (wire type 2, length-delimited -- same framing
/// as `string`, protobuf doesn't distinguish them on the wire).
fn encode_bytes_field(field_number: u32, value: &[u8]) -> BytesMut {
    let mut out = BytesMut::new();
    encode_varint(((field_number as u64) << 3) | 2, &mut out);
    encode_varint(value.len() as u64, &mut out);
    out.extend_from_slice(value);
    out
}

/// Encode a varint-typed field (wire type 0 -- `int32`/`bool`/enum
/// values all share this wire representation).
fn encode_varint_field(field_number: u32, value: u64, out: &mut BytesMut) {
    encode_varint((field_number as u64) << 3, out);
    encode_varint(value, out);
}

/// If `bytes` is a `ServerReflectionRequest` with the
/// `file_containing_symbol` oneof field (field 4) set, return that
/// symbol name. Parsing mirrors `request_is_list_services` (field 7)
/// but extracts the string payload instead of just detecting presence.
fn extract_file_containing_symbol(bytes: &[u8]) -> Option<String> {
    let mut buf = bytes;
    while !buf.is_empty() {
        let (tag, rest) = decode_varint(buf)?;
        let field_number = tag >> 3;
        let wire_type = tag & 0x7;
        buf = rest;
        match wire_type {
            2 => {
                let (len, rest) = decode_varint(buf)?;
                let len = len as usize;
                if rest.len() < len {
                    return None;
                }
                if field_number == 4 {
                    return Some(String::from_utf8_lossy(&rest[..len]).into_owned());
                }
                buf = &rest[len..];
            }
            0 => {
                let (_, rest) = decode_varint(buf)?;
                buf = rest;
            }
            _ => return None,
        }
    }
    None
}

/// If `bytes` is a `ServerReflectionRequest` with the `file_by_filename`
/// oneof field (field 3) set, return that filename. Parsing mirrors
/// [`extract_file_containing_symbol`] (field 4) but reads field 3
/// instead -- these two oneof variants share the same wire type
/// (length-delimited string), only the field number differs.
fn extract_file_by_filename(bytes: &[u8]) -> Option<String> {
    let mut buf = bytes;
    while !buf.is_empty() {
        let (tag, rest) = decode_varint(buf)?;
        let field_number = tag >> 3;
        let wire_type = tag & 0x7;
        buf = rest;
        match wire_type {
            2 => {
                let (len, rest) = decode_varint(buf)?;
                let len = len as usize;
                if rest.len() < len {
                    return None;
                }
                if field_number == 3 {
                    return Some(String::from_utf8_lossy(&rest[..len]).into_owned());
                }
                buf = &rest[len..];
            }
            0 => {
                let (_, rest) = decode_varint(buf)?;
                buf = rest;
            }
            _ => return None,
        }
    }
    None
}

/// A hand-rolled `FileDescriptorProto` (per
/// [`descriptor.proto`](https://github.com/protocolbuffers/protobuf/blob/main/src/google/protobuf/descriptor.proto))
/// describing `grpc/health/v1/health.proto`: the `Health` service
/// (`Check`/`Watch` methods) and its two message types. Built with the
/// same varint/length-delimited primitives as the rest of this module's
/// hand-rolled codec -- no `prost`/protoc dependency.
///
/// **Simplification, stated plainly**: `HealthCheckResponse.status` is
/// encoded here as `TYPE_INT32` (wire type 5 in `FieldDescriptorProto`)
/// rather than the real `ServingStatus` enum type, which would require
/// also emitting an `EnumDescriptorProto` and a `type_name` cross-
/// reference. A reflection client decoding this descriptor sees a
/// structurally valid message with an integer status field instead of a
/// named enum -- correct on the wire, less self-documenting than the
/// real `health.proto`. This module's actual `Check`/`Watch` responses
/// are unaffected (they never go through this descriptor, only through
/// `encode_health_check_response`).
fn build_health_file_descriptor_proto() -> Bytes {
    const TYPE_INT32: u64 = 5;
    const TYPE_STRING: u64 = 9;
    const LABEL_OPTIONAL: u64 = 1;

    let field_descriptor = |name: &str, number: u64, proto_type: u64| -> BytesMut {
        let mut out = BytesMut::new();
        out.extend_from_slice(&encode_string_field(1, name));
        encode_varint_field(3, number, &mut out);
        encode_varint_field(4, LABEL_OPTIONAL, &mut out);
        encode_varint_field(5, proto_type, &mut out);
        out
    };
    let message_descriptor = |name: &str, fields: &[BytesMut]| -> BytesMut {
        let mut out = BytesMut::new();
        out.extend_from_slice(&encode_string_field(1, name));
        for f in fields {
            out.extend_from_slice(&encode_embedded_message(2, f));
        }
        out
    };
    let method_descriptor = |name: &str, input_type: &str, output_type: &str, server_streaming: bool| -> BytesMut {
        let mut out = BytesMut::new();
        out.extend_from_slice(&encode_string_field(1, name));
        out.extend_from_slice(&encode_string_field(2, input_type));
        out.extend_from_slice(&encode_string_field(3, output_type));
        if server_streaming {
            encode_varint_field(6, 1, &mut out);
        }
        out
    };

    let request_msg = message_descriptor("HealthCheckRequest", &[field_descriptor("service", 1, TYPE_STRING)]);
    let response_msg = message_descriptor("HealthCheckResponse", &[field_descriptor("status", 1, TYPE_INT32)]);
    let check_method = method_descriptor(
        "Check",
        ".grpc.health.v1.HealthCheckRequest",
        ".grpc.health.v1.HealthCheckResponse",
        false,
    );
    let watch_method = method_descriptor(
        "Watch",
        ".grpc.health.v1.HealthCheckRequest",
        ".grpc.health.v1.HealthCheckResponse",
        true,
    );
    let mut service = BytesMut::new();
    service.extend_from_slice(&encode_string_field(1, "Health"));
    service.extend_from_slice(&encode_embedded_message(2, &check_method));
    service.extend_from_slice(&encode_embedded_message(2, &watch_method));

    let mut file = BytesMut::new();
    file.extend_from_slice(&encode_string_field(1, "grpc/health/v1/health.proto"));
    file.extend_from_slice(&encode_string_field(2, "grpc.health.v1"));
    file.extend_from_slice(&encode_embedded_message(4, &request_msg));
    file.extend_from_slice(&encode_embedded_message(4, &response_msg));
    file.extend_from_slice(&encode_embedded_message(6, &service));
    file.extend_from_slice(&encode_string_field(12, "proto3"));
    file.freeze()
}

/// Resolve a `file_containing_symbol` request's symbol name to the
/// `FileDescriptorProto` bytes of the file that defines it, or `None` if
/// this server doesn't recognize the symbol (the caller should respond
/// `NOT_FOUND`, mirroring how `Check`/`Watch` already report `NOT_FOUND`
/// for a health-check request naming an unknown service).
fn resolve_symbol_file_descriptor(symbol: &str) -> Option<Bytes> {
    const HEALTH_SYMBOLS: &[&str] = &[
        "grpc.health.v1.Health",
        "grpc.health.v1.Health.Check",
        "grpc.health.v1.Health.Watch",
        "grpc.health.v1.HealthCheckRequest",
        "grpc.health.v1.HealthCheckResponse",
    ];
    if HEALTH_SYMBOLS.contains(&symbol) {
        Some(build_health_file_descriptor_proto())
    } else {
        None
    }
}

/// Resolve a `file_by_filename` request's proto filename to the
/// `FileDescriptorProto` bytes of that file, or `None` if this server
/// doesn't recognize the filename. Unlike [`resolve_symbol_file_descriptor`]
/// (which resolves *symbols* -- service/method/message names), this
/// resolves the file path itself -- `grpcurl <addr> descriptor
/// grpc/health/v1/health.proto` (as opposed to `describe
/// grpc.health.v1.Health`) is the client-facing use case this closes.
fn resolve_filename_file_descriptor(filename: &str) -> Option<Bytes> {
    const HEALTH_PROTO_FILENAME: &str = "grpc/health/v1/health.proto";
    if filename == HEALTH_PROTO_FILENAME {
        Some(build_health_file_descriptor_proto())
    } else {
        None
    }
}

/// Build the `ServerReflectionResponse` bytes for a successful
/// `file_containing_symbol` request: field 4 (`file_descriptor_response`)
/// wraps a `FileDescriptorResponse { repeated bytes file_descriptor_proto = 1; }`.
fn encode_file_descriptor_response(file_descriptor_proto: &[u8]) -> Bytes {
    let mut fdr = BytesMut::new();
    fdr.extend_from_slice(&encode_bytes_field(1, file_descriptor_proto));
    let response = encode_embedded_message(4, &fdr);
    response.freeze()
}

/// `grpc.health.v1.HealthCheckRequest { string service = 1; }` -- the
/// `service` field is optional in real health-check clients (empty string
/// means "overall server health"); this decoder tolerates a completely
/// empty message the same way.
struct HealthCheckRequest {
    service: String,
}

fn decode_health_check_request(bytes: &[u8]) -> Result<HealthCheckRequest, GrpcStatus> {
    let mut service = String::new();
    let mut buf = bytes;
    while !buf.is_empty() {
        let Some((tag, rest)) = decode_varint(buf) else {
            return Err(GrpcStatus::InvalidArgument);
        };
        let field_number = tag >> 3;
        let wire_type = tag & 0x7;
        buf = rest;
        match (field_number, wire_type) {
            (1, 2) => {
                let Some((len, rest)) = decode_varint(buf) else {
                    return Err(GrpcStatus::InvalidArgument);
                };
                let len = len as usize;
                let Some(value_bytes) = rest.get(..len) else {
                    return Err(GrpcStatus::InvalidArgument);
                };
                service = String::from_utf8_lossy(value_bytes).into_owned();
                buf = &rest[len..];
            }
            // Unknown field: skip it minimally-correctly for the two wire
            // types this tiny decoder might plausibly see; anything else
            // is rejected rather than silently mis-parsed.
            (_, 0) => {
                let Some((_, rest)) = decode_varint(buf) else {
                    return Err(GrpcStatus::InvalidArgument);
                };
                buf = rest;
            }
            (_, 2) => {
                let Some((len, rest)) = decode_varint(buf) else {
                    return Err(GrpcStatus::InvalidArgument);
                };
                let len = len as usize;
                if rest.len() < len {
                    return Err(GrpcStatus::InvalidArgument);
                }
                buf = &rest[len..];
            }
            _ => return Err(GrpcStatus::InvalidArgument),
        }
    }
    Ok(HealthCheckRequest { service })
}

/// `grpc.health.v1.HealthCheckResponse.ServingStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServingStatus {
    Unknown = 0,
    Serving = 1,
    NotServing = 2,
}

/// `grpc.health.v1.HealthCheckResponse { ServingStatus status = 1; }`.
fn encode_health_check_response(status: ServingStatus) -> Bytes {
    let mut out = BytesMut::new();
    encode_varint((1 << 3) | 0, &mut out); // field 1, wire type 0 (varint)
    encode_varint(status as u64, &mut out);
    out.freeze()
}

/// Client-side counterpart of [`decode_health_check_request`]: encode a
/// `HealthCheckRequest { string service = 1; }` to send. Only used by
/// [`check_remote_health`] (this module's server-side handlers receive
/// already-encoded requests from real clients; they never need to build
/// one themselves).
fn encode_health_check_request(service: &str) -> Bytes {
    if service.is_empty() {
        return Bytes::new();
    }
    encode_string_field_bytes(1, service)
}

/// Same wire encoding as `grpc.rs`'s `encode_string_field` (module-private
/// duplication is intentional -- this file has no shared "protobuf
/// helpers" module yet, and the two encoders serve different call sites:
/// this one for building a *request* to send, the other for building a
/// *response*).
fn encode_string_field_bytes(field_number: u32, value: &str) -> Bytes {
    let mut out = BytesMut::new();
    encode_varint(((field_number as u64) << 3) | 2, &mut out);
    encode_varint(value.len() as u64, &mut out);
    out.extend_from_slice(value.as_bytes());
    out.freeze()
}

/// Client-side counterpart of [`encode_health_check_response`]: decode a
/// `HealthCheckResponse { ServingStatus status = 1; }` received from a
/// server. Tolerates a missing `status` field (protobuf's "absent means
/// default" rule) by defaulting to `Unknown`, same as a real generated
/// client would.
fn decode_health_check_response(bytes: &[u8]) -> Result<ServingStatus, String> {
    let mut status = ServingStatus::Unknown;
    let mut buf = bytes;
    while !buf.is_empty() {
        let Some((tag, rest)) = decode_varint(buf) else {
            return Err("malformed HealthCheckResponse: bad tag varint".to_string());
        };
        let field_number = tag >> 3;
        let wire_type = tag & 0x7;
        buf = rest;
        if field_number == 1 && wire_type == 0 {
            let Some((value, rest)) = decode_varint(buf) else {
                return Err("malformed HealthCheckResponse: bad status varint".to_string());
            };
            status = match value {
                1 => ServingStatus::Serving,
                2 => ServingStatus::NotServing,
                _ => ServingStatus::Unknown,
            };
            buf = rest;
        } else {
            return Err(format!(
                "unsupported field in HealthCheckResponse: field {field_number}, wire type {wire_type}"
            ));
        }
    }
    Ok(status)
}

/// Call a `grpc.health.v1.Health/Check` RPC against **any** compliant gRPC
/// server -- this process's own (via `OPEN_RUNO_GRPC_BIND_ADDR`) or a
/// genuinely external one -- and return its reported [`ServingStatus`].
///
/// This is the "Cosmo Connect" Cosmo-parity gap (`docs/cosmo-parity.md`
/// §4a, "gRPC対応"): bringing an existing gRPC service into the GraphQL
/// layer as a queryable field, rather than requiring every consumer to
/// speak gRPC directly. `open-runo-gateway`'s GraphQL schema exposes this
/// as the `grpcHealthCheck(endpoint, service)` query field. Scoped
/// deliberately small: only the one well-known `Health` service, not
/// Cosmo Connect's full "any `.proto`-described service, dynamically
/// composed into the schema" generality -- that would require a real
/// protobuf/reflection-driven schema generator, a substantially larger
/// undertaking than a single pass can responsibly deliver and verify.
///
/// `addr` is a plain `host:port` (h2c, no TLS, matching every other gRPC
/// endpoint in this module). Returns `Err` for connection failures,
/// non-OK `grpc-status`, or malformed responses -- never panics.
pub async fn check_remote_health(addr: &str, service: &str) -> Result<ServingStatus, String> {
    use http_body_util::Full;
    use hyper_util::client::legacy::connect::HttpConnector;
    use hyper_util::client::legacy::Client;
    use hyper_util::rt::TokioExecutor;

    let mut connector = HttpConnector::new();
    connector.enforce_http(false);
    let client: Client<_, Full<Bytes>> =
        Client::builder(TokioExecutor::new()).http2_only(true).build(connector);

    let request_message = encode_health_check_request(service);
    let request_body = encode_grpc_frame(&request_message);

    let req = HyperRequest::builder()
        .method("POST")
        .uri(format!("http://{addr}/grpc.health.v1.Health/Check"))
        .header("content-type", "application/grpc+proto")
        .body(Full::new(request_body))
        .map_err(|e| format!("failed to build gRPC request: {e}"))?;

    let resp = client
        .request(req)
        .await
        .map_err(|e| format!("gRPC connection to {addr} failed: {e}"))?;

    let collected = resp
        .collect()
        .await
        .map_err(|e| format!("failed to read gRPC response body/trailers: {e}"))?;

    let trailers = collected.trailers().cloned().unwrap_or_default();
    let grpc_status: i32 = trailers
        .get("grpc-status")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(2); // UNKNOWN if the trailer is missing entirely
    if grpc_status != GrpcStatus::Ok as i32 {
        return Err(format!("remote service returned grpc-status {grpc_status}"));
    }

    let body_bytes = collected.to_bytes();
    let message = decode_grpc_frame(&body_bytes)
        .map_err(|status| format!("malformed gRPC response frame (status {})", status as i32))?;
    decode_health_check_response(message)
}

// ── HTTP/2 <-> gRPC glue ──────────────────────────────────────────────────

/// Build a gRPC-framed, trailers-terminated unary response: one DATA frame
/// (the encoded message) followed by a TRAILERS frame carrying
/// `grpc-status` (and `grpc-message` on failure). Per
/// [the gRPC-over-HTTP2 spec](https://github.com/grpc/grpc/blob/master/doc/PROTOCOL-HTTP2.md#responses),
/// `grpc-status` is always a trailer, never a leading header, even for a
/// successful unary call.
fn grpc_response(status: GrpcStatus, message: Option<&[u8]>) -> Response {
    let mut frames: Vec<Result<Frame<Bytes>, Infallible>> = Vec::with_capacity(2);
    if let Some(message) = message {
        frames.push(Ok(Frame::data(encode_grpc_frame(message))));
    }
    let mut trailers = HeaderMap::new();
    trailers.insert("grpc-status", (status as i32).to_string().parse().unwrap());
    trailers.insert("grpc-message", "".parse().unwrap());
    frames.push(Ok(Frame::trailers(trailers)));

    let body: BoxBody<Bytes, Infallible> = BodyExt::boxed(StreamBody::new(futures::stream::iter(frames)));
    HyperResponse::builder()
        .status(StatusCode::OK) // gRPC status is orthogonal to the HTTP status -- 200 even on grpc-status != 0
        .header("content-type", "application/grpc+proto")
        .body(body)
        .expect("building a response from a fixed set of valid headers cannot fail")
}

/// Build a gRPC-framed, trailers-terminated **streaming** response: zero or
/// more DATA frames (one per entry in `messages`) followed by a TRAILERS
/// frame carrying `grpc-status`. This generalizes [`grpc_response`] for
/// server-streaming RPCs (e.g. `Watch`) where more than one message may be
/// sent before the stream completes -- unary RPCs keep using
/// `grpc_response` (0-or-1 message) unchanged.
fn grpc_streaming_response(status: GrpcStatus, messages: &[Bytes]) -> Response {
    let mut frames: Vec<Result<Frame<Bytes>, Infallible>> = Vec::with_capacity(messages.len() + 1);
    for message in messages {
        frames.push(Ok(Frame::data(encode_grpc_frame(message))));
    }
    let mut trailers = HeaderMap::new();
    trailers.insert("grpc-status", (status as i32).to_string().parse().unwrap());
    trailers.insert("grpc-message", "".parse().unwrap());
    frames.push(Ok(Frame::trailers(trailers)));

    let body: BoxBody<Bytes, Infallible> = BodyExt::boxed(StreamBody::new(futures::stream::iter(frames)));
    HyperResponse::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/grpc+proto")
        .body(body)
        .expect("building a response from a fixed set of valid headers cannot fail")
}

/// The full gRPC service names this server actually exposes, per the
/// `Health` service's contract: an empty `service` name in a
/// `HealthCheckRequest` means "overall server health" (always answered),
/// but a *named* service that this server doesn't expose must return
/// `NOT_FOUND` (5) rather than silently claiming `SERVING` -- otherwise a
/// health-checking client (a load balancer, `grpc-health-probe`, etc.)
/// could be told a service is healthy when it doesn't exist at all.
const KNOWN_SERVICES: &[&str] = &[
    "grpc.health.v1.Health",
    "grpc.reflection.v1.ServerReflection",
];

/// `POST /grpc.health.v1.Health/Check` -- the one real unary RPC this
/// service implements. Always reports `SERVING` for a known service name
/// (this server has no notion of per-dependency health beyond "the
/// process is up and answering"); wiring in real subsystem checks (DB
/// connectivity, etc.) is a natural follow-up once more of `AppState`
/// needs to be reachable from here.
async fn health_check_handler(req: Request) -> Response {
    let collected = match req.into_body().collect().await {
        Ok(c) => c.to_bytes(),
        Err(_) => return grpc_response(GrpcStatus::Internal, None),
    };
    let message = match decode_grpc_frame(&collected) {
        Ok(m) => m,
        Err(status) => return grpc_response(status, None),
    };
    let request = match decode_health_check_request(message) {
        Ok(r) => r,
        Err(_) => return grpc_response(GrpcStatus::InvalidArgument, None),
    };
    if !request.service.is_empty() && !KNOWN_SERVICES.contains(&request.service.as_str()) {
        return grpc_response(GrpcStatus::NotFound, None);
    }
    let response_bytes = encode_health_check_response(ServingStatus::Serving);
    grpc_response(GrpcStatus::Ok, Some(&response_bytes))
}

/// `POST /grpc.health.v1.Health/Watch` -- the server-streaming counterpart
/// to `Check`, closing the "no streaming" gap this module previously
/// documented. Real `Watch` implementations push a new
/// `HealthCheckResponse` each time the serving status changes and hold the
/// stream open indefinitely; this server's status never changes (always
/// `SERVING` for a known service, `NOT_FOUND` for an unknown one, same as
/// `Check`), so the minimal spec-compliant behavior is to send exactly
/// one message with the current status and then complete the stream --
/// a real streaming client (`grpcurl`, `grpc-health-probe --watch`, etc.)
/// gets a well-formed streaming response with real HTTP/2 DATA + TRAILERS
/// frames, rather than an `UNIMPLEMENTED` error.
async fn watch_handler(req: Request) -> Response {
    let collected = match req.into_body().collect().await {
        Ok(c) => c.to_bytes(),
        Err(_) => return grpc_response(GrpcStatus::Internal, None),
    };
    let message = match decode_grpc_frame(&collected) {
        Ok(m) => m,
        Err(status) => return grpc_response(status, None),
    };
    let request = match decode_health_check_request(message) {
        Ok(r) => r,
        Err(_) => return grpc_response(GrpcStatus::InvalidArgument, None),
    };
    if !request.service.is_empty() && !KNOWN_SERVICES.contains(&request.service.as_str()) {
        return grpc_response(GrpcStatus::NotFound, None);
    }
    let response_bytes = encode_health_check_response(ServingStatus::Serving);
    grpc_streaming_response(GrpcStatus::Ok, &[response_bytes])
}

/// `POST /grpc.reflection.v1.ServerReflection/ServerReflectionInfo` --
/// closes the "no reflection" gap this module's doc comment previously
/// called out. The real spec defines this as a **bidirectional**
/// streaming RPC (a client can send several requests over one stream --
/// `list_services`, then `file_containing_symbol`, etc. -- and get a
/// response for each). This implementation deliberately only handles a
/// single request per stream (exactly one request, one response, then
/// the stream completes) rather than the full bidirectional exchange --
/// but within that constraint, it now answers `list_services` (service
/// discovery, e.g. `grpcurl <addr> list`), `file_containing_symbol`
/// (schema introspection, e.g. `grpcurl <addr> describe
/// grpc.health.v1.Health`), and -- added 2026-07-18 -- `file_by_filename`
/// (e.g. `grpcurl <addr> descriptor grpc/health/v1/health.proto`) for the
/// services this server actually implements. Extension queries
/// (`file_containing_extension`, `all_extension_numbers_of_type`) are
/// still answered with `UNIMPLEMENTED` -- protobuf extensions aren't a
/// concept this server's hand-rolled messages use at all, so there is no
/// meaningful answer to give.
async fn reflection_handler(req: Request) -> Response {
    let collected = match req.into_body().collect().await {
        Ok(c) => c.to_bytes(),
        Err(_) => return grpc_response(GrpcStatus::Internal, None),
    };
    let message = match decode_grpc_frame(&collected) {
        Ok(m) => m,
        Err(status) => return grpc_response(status, None),
    };
    if request_is_list_services(message) {
        let response_bytes = encode_list_services_response(KNOWN_SERVICES);
        return grpc_streaming_response(GrpcStatus::Ok, &[response_bytes]);
    }
    if let Some(symbol) = extract_file_containing_symbol(message) {
        return match resolve_symbol_file_descriptor(&symbol) {
            Some(file_descriptor_proto) => {
                let response_bytes = encode_file_descriptor_response(&file_descriptor_proto);
                grpc_streaming_response(GrpcStatus::Ok, &[response_bytes])
            }
            // Recognized request kind, unknown symbol -- e.g. asking
            // about a service this server doesn't implement.
            None => grpc_response(GrpcStatus::NotFound, None),
        };
    }
    if let Some(filename) = extract_file_by_filename(message) {
        return match resolve_filename_file_descriptor(&filename) {
            Some(file_descriptor_proto) => {
                let response_bytes = encode_file_descriptor_response(&file_descriptor_proto);
                grpc_streaming_response(GrpcStatus::Ok, &[response_bytes])
            }
            // Recognized request kind, unknown filename -- e.g. asking
            // about a .proto file this server doesn't serve.
            None => grpc_response(GrpcStatus::NotFound, None),
        };
    }
    // A real, recognized-but-unsupported reflection request kind
    // (extension queries).
    grpc_response(GrpcStatus::Unimplemented, None)
}

/// Serve the gRPC health-check service over real HTTP/2 (h2c, prior
/// knowledge -- no TLS required, matching how most internal/dev gRPC
/// traffic runs). Returns the bound address and a task handle, same
/// shape as `hyper_compat::serve`/`hyper_compat::tls::serve_tls`.
pub async fn serve_grpc(addr: std::net::SocketAddr) -> std::io::Result<(std::net::SocketAddr, tokio::task::JoinHandle<()>)> {
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind(addr).await?;
    let bound_addr = listener.local_addr()?;

    let handle = tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(pair) => pair,
                Err(_) => continue,
            };
            let io = TokioIo::new(stream);
            tokio::spawn(async move {
                let service = hyper::service::service_fn(move |req: Request| async move {
                    let path = req.uri().path();
                    let resp = if path == "/grpc.health.v1.Health/Check" {
                        health_check_handler(req).await
                    } else if path == "/grpc.health.v1.Health/Watch" {
                        watch_handler(req).await
                    } else if path == "/grpc.reflection.v1.ServerReflection/ServerReflectionInfo" {
                        reflection_handler(req).await
                    } else {
                        grpc_response(GrpcStatus::Unimplemented, None)
                    };
                    Ok::<Response, Infallible>(resp)
                });
                let _ = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
                    .serve_connection(io, service)
                    .await;
            });
        }
    });

    Ok((bound_addr, handle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_round_trips_small_and_large_values() {
        for value in [0u64, 1, 127, 128, 300, 16384, u32::MAX as u64] {
            let mut buf = BytesMut::new();
            encode_varint(value, &mut buf);
            let (decoded, rest) = decode_varint(&buf).unwrap();
            assert_eq!(decoded, value);
            assert!(rest.is_empty());
        }
    }

    #[test]
    fn grpc_frame_round_trips() {
        let message = b"hello grpc";
        let framed = encode_grpc_frame(message);
        let decoded = decode_grpc_frame(&framed).unwrap();
        assert_eq!(decoded, message);
    }

    #[test]
    fn decode_grpc_frame_rejects_truncated_input() {
        assert_eq!(decode_grpc_frame(&[0, 0, 0]), Err(GrpcStatus::InvalidArgument));
    }

    #[test]
    fn decode_grpc_frame_rejects_compressed_flag() {
        let mut framed = encode_grpc_frame(b"x").to_vec();
        framed[0] = 1; // claim compression, which this server never negotiates
        assert_eq!(decode_grpc_frame(&framed), Err(GrpcStatus::Unimplemented));
    }

    #[test]
    fn health_check_response_encodes_serving_as_field_1_value_1() {
        let bytes = encode_health_check_response(ServingStatus::Serving);
        // field 1 (status), wire type 0 (varint) -> tag byte 0x08, then
        // varint(1) -> single byte 0x01. This is exactly what any real
        // protobuf decoder (protoc, prost, etc.) would produce/expect for
        // `HealthCheckResponse { status: SERVING }`.
        assert_eq!(bytes.as_ref(), &[0x08, 0x01]);
    }

    #[test]
    fn client_request_response_codec_round_trips() {
        // encode_health_check_request / decode_health_check_response are
        // the client-side counterparts of this module's existing
        // server-side decode_health_check_request / encode_health_check_response
        // -- confirm they agree with each other (and, by construction,
        // with the server side, since both pairs use the same wire
        // format).
        let req_bytes = encode_health_check_request("orders");
        let decoded_req = decode_health_check_request(&req_bytes).unwrap();
        assert_eq!(decoded_req.service, "orders");

        let resp_bytes = encode_health_check_response(ServingStatus::NotServing);
        let decoded_resp = decode_health_check_response(&resp_bytes).unwrap();
        assert_eq!(decoded_resp, ServingStatus::NotServing);
    }

    #[test]
    fn encode_health_check_request_empty_service_is_empty_message() {
        // Per the protobuf "absent means default" rule, an empty service
        // name should serialize to zero bytes (the field is simply
        // omitted), matching how a real client's codegen would behave.
        assert_eq!(encode_health_check_request("").len(), 0);
    }

    #[test]
    fn decode_health_check_request_accepts_empty_message() {
        // A HealthCheckRequest with no `service` field set serializes to
        // zero bytes in protobuf -- decoding that must succeed with the
        // default (empty) value, not error.
        let req = decode_health_check_request(&[]).unwrap();
        assert_eq!(req.service, "");
    }

    #[test]
    fn decode_health_check_request_parses_service_field() {
        // Hand-encode `HealthCheckRequest { service: "orders" }`: tag
        // (field 1, wire type 2) = 0x0a, length = 6, then the UTF-8 bytes.
        let mut bytes = vec![0x0a, 6];
        bytes.extend_from_slice(b"orders");
        let req = decode_health_check_request(&bytes).unwrap();
        assert_eq!(req.service, "orders");
    }

    #[test]
    fn encode_string_field_matches_hand_rolled_expectation() {
        // string field 1 = "hi": tag 0x0a, len 2, then "hi".
        let out = encode_string_field(1, "hi");
        assert_eq!(out.as_ref(), &[0x0a, 2, b'h', b'i']);
    }

    #[test]
    fn request_is_list_services_detects_field_7() {
        // ServerReflectionRequest { list_services: "" } -> field 7, wire
        // type 2, tag = (7 << 3) | 2 = 0x3a, then a zero-length string.
        let bytes = [0x3a, 0x00];
        assert!(request_is_list_services(&bytes));
    }

    #[test]
    fn request_is_list_services_rejects_other_oneof_fields() {
        // ServerReflectionRequest { file_by_filename: "x" } -> field 3,
        // tag = (3 << 3) | 2 = 0x1a, len 1, "x".
        let bytes = [0x1a, 1, b'x'];
        assert!(!request_is_list_services(&bytes));
    }

    #[test]
    fn encode_list_services_response_contains_both_service_names() {
        let bytes = encode_list_services_response(KNOWN_SERVICES);
        // Sanity check by looking for the raw UTF-8 bytes of both names
        // in the encoded output -- a full round-trip decoder isn't
        // implemented here (this module only needs to encode responses,
        // never decode reflection responses), but this confirms both
        // names were actually serialized, not silently dropped.
        let as_str = String::from_utf8_lossy(&bytes);
        assert!(as_str.contains("grpc.health.v1.Health"));
        assert!(as_str.contains("grpc.reflection.v1.ServerReflection"));
    }

    /// End-to-end: a real HTTP/2 server (h2c, prior knowledge) answering a
    /// real gRPC unary call, verified with hyper's own HTTP/2 client --
    /// not just a same-process function call. Proves the framing,
    /// trailers-based grpc-status, and protobuf encode/decode all agree
    /// with each other over an actual TCP connection.
    #[tokio::test]
    async fn health_check_serves_over_real_http2() {
        use http_body_util::{BodyExt, Full};
        use hyper_util::client::legacy::connect::HttpConnector;
        use hyper_util::client::legacy::Client;
        use hyper_util::rt::TokioExecutor;

        let (addr, _handle) = serve_grpc("127.0.0.1:0".parse().unwrap()).await.expect("bind grpc port");

        let mut connector = HttpConnector::new();
        connector.enforce_http(false);
        let client: Client<_, Full<Bytes>> =
            Client::builder(TokioExecutor::new()).http2_only(true).build(connector);

        // Empty HealthCheckRequest (no `service` field set).
        let request_body = encode_grpc_frame(&[]);
        let req = HyperRequest::builder()
            .method("POST")
            .uri(format!("http://{addr}/grpc.health.v1.Health/Check"))
            .header("content-type", "application/grpc+proto")
            .body(Full::new(request_body))
            .unwrap();

        let resp = client.request(req).await.expect("HTTP/2 request should succeed");
        assert_eq!(resp.status(), StatusCode::OK);

        let collected = resp.collect().await.expect("collecting body+trailers should succeed");
        let trailers = collected.trailers().cloned().unwrap_or_default();
        assert_eq!(
            trailers.get("grpc-status").and_then(|v| v.to_str().ok()),
            Some("0"),
            "grpc-status should be OK (0), carried as an HTTP/2 trailer"
        );

        let body_bytes = collected.to_bytes();
        let message = decode_grpc_frame(&body_bytes).expect("response should be a valid gRPC frame");
        // HealthCheckResponse { status: SERVING } -> [0x08, 0x01], per the
        // dedicated unit test above.
        assert_eq!(message, &[0x08, 0x01]);

        // Also exercise the "unknown method" path over the same real
        // connection, to prove routing (not just the one happy-path RPC)
        // works end to end.
        let req2 = HyperRequest::builder()
            .method("POST")
            .uri(format!("http://{addr}/no.such.Service/Method"))
            .body(Full::new(Bytes::new()))
            .unwrap();
        let resp2 = client.request(req2).await.expect("HTTP/2 request should succeed");
        let trailers2 = resp2.collect().await.unwrap().trailers().cloned().unwrap_or_default();
        assert_eq!(trailers2.get("grpc-status").and_then(|v| v.to_str().ok()), Some("12")); // UNIMPLEMENTED
    }

    /// End-to-end test for the server-streaming `Watch` RPC, over the same
    /// real HTTP/2 transport as the unary `Check` test above. Confirms the
    /// "no streaming" gap is closed: a real streaming RPC (DATA frame(s)
    /// followed by TRAILERS) works, not just a same-process function call.
    #[tokio::test]
    async fn watch_streams_current_status_over_real_http2() {
        use http_body_util::{BodyExt, Full};
        use hyper_util::client::legacy::connect::HttpConnector;
        use hyper_util::client::legacy::Client;
        use hyper_util::rt::TokioExecutor;

        let (addr, _handle) = serve_grpc("127.0.0.1:0".parse().unwrap()).await.expect("bind grpc port");

        let mut connector = HttpConnector::new();
        connector.enforce_http(false);
        let client: Client<_, Full<Bytes>> =
            Client::builder(TokioExecutor::new()).http2_only(true).build(connector);

        let request_body = encode_grpc_frame(&[]);
        let req = HyperRequest::builder()
            .method("POST")
            .uri(format!("http://{addr}/grpc.health.v1.Health/Watch"))
            .header("content-type", "application/grpc+proto")
            .body(Full::new(request_body))
            .unwrap();

        let resp = client.request(req).await.expect("HTTP/2 request should succeed");
        assert_eq!(resp.status(), StatusCode::OK);

        let collected = resp.collect().await.expect("collecting body+trailers should succeed");
        let trailers = collected.trailers().cloned().unwrap_or_default();
        assert_eq!(
            trailers.get("grpc-status").and_then(|v| v.to_str().ok()),
            Some("0"),
            "grpc-status should be OK (0) even for a streaming RPC, carried as an HTTP/2 trailer"
        );

        let body_bytes = collected.to_bytes();
        let message = decode_grpc_frame(&body_bytes)
            .expect("streamed response should contain at least one valid gRPC-framed message");
        // Same encoding as the unary Check test: HealthCheckResponse { status: SERVING }.
        assert_eq!(message, &[0x08, 0x01]);
    }

    /// End-to-end: `Check` against an unrecognized named service must
    /// return `NOT_FOUND` (5), not silently claim `SERVING`. Real client
    /// (a load balancer, `grpc-health-probe --service=nonexistent`) over
    /// a real HTTP/2 connection.
    #[tokio::test]
    async fn check_returns_not_found_for_unknown_service() {
        use http_body_util::{BodyExt, Full};
        use hyper_util::client::legacy::connect::HttpConnector;
        use hyper_util::client::legacy::Client;
        use hyper_util::rt::TokioExecutor;

        let (addr, _handle) = serve_grpc("127.0.0.1:0".parse().unwrap()).await.expect("bind grpc port");

        let mut connector = HttpConnector::new();
        connector.enforce_http(false);
        let client: Client<_, Full<Bytes>> =
            Client::builder(TokioExecutor::new()).http2_only(true).build(connector);

        // HealthCheckRequest { service: "nonexistent.Service" }
        let mut service_field = vec![0x0a, 19]; // tag(field 1, wire 2), len 19
        service_field.extend_from_slice(b"nonexistent.Service");
        let request_body = encode_grpc_frame(&service_field);

        let req = HyperRequest::builder()
            .method("POST")
            .uri(format!("http://{addr}/grpc.health.v1.Health/Check"))
            .header("content-type", "application/grpc+proto")
            .body(Full::new(request_body))
            .unwrap();

        let resp = client.request(req).await.expect("HTTP/2 request should succeed");
        let trailers = resp.collect().await.unwrap().trailers().cloned().unwrap_or_default();
        assert_eq!(
            trailers.get("grpc-status").and_then(|v| v.to_str().ok()),
            Some("5"), // NOT_FOUND
            "checking an unknown service name must return NOT_FOUND, not SERVING"
        );
    }

    /// End-to-end: `grpc.reflection.v1.ServerReflection/ServerReflectionInfo`
    /// answering a `list_services` request, over a real HTTP/2 connection
    /// with a real independent client -- confirms the "no reflection" gap
    /// is closed for the common case (`grpcurl <addr> list`).
    #[tokio::test]
    async fn reflection_lists_known_services_over_real_http2() {
        use http_body_util::{BodyExt, Full};
        use hyper_util::client::legacy::connect::HttpConnector;
        use hyper_util::client::legacy::Client;
        use hyper_util::rt::TokioExecutor;

        let (addr, _handle) = serve_grpc("127.0.0.1:0".parse().unwrap()).await.expect("bind grpc port");

        let mut connector = HttpConnector::new();
        connector.enforce_http(false);
        let client: Client<_, Full<Bytes>> =
            Client::builder(TokioExecutor::new()).http2_only(true).build(connector);

        // ServerReflectionRequest { list_services: "" } -> field 7, tag 0x3a, empty string.
        let request_message = [0x3a, 0x00];
        let request_body = encode_grpc_frame(&request_message);

        let req = HyperRequest::builder()
            .method("POST")
            .uri(format!(
                "http://{addr}/grpc.reflection.v1.ServerReflection/ServerReflectionInfo"
            ))
            .header("content-type", "application/grpc+proto")
            .body(Full::new(request_body))
            .unwrap();

        let resp = client.request(req).await.expect("HTTP/2 request should succeed");
        assert_eq!(resp.status(), StatusCode::OK);

        let collected = resp.collect().await.expect("collecting body+trailers should succeed");
        let trailers = collected.trailers().cloned().unwrap_or_default();
        assert_eq!(
            trailers.get("grpc-status").and_then(|v| v.to_str().ok()),
            Some("0"),
            "list_services should succeed"
        );

        let body_bytes = collected.to_bytes();
        let message = decode_grpc_frame(&body_bytes)
            .expect("reflection response should be a valid gRPC-framed message");
        let as_str = String::from_utf8_lossy(&message);
        assert!(
            as_str.contains("grpc.health.v1.Health"),
            "listed services should include the Health service"
        );
        assert!(
            as_str.contains("grpc.reflection.v1.ServerReflection"),
            "listed services should include the reflection service itself"
        );
    }

    /// End-to-end: a reflection request kind this implementation doesn't
    /// support (`file_containing_extension`) must return `UNIMPLEMENTED`,
    /// not be silently mishandled as if it were `list_services`. (Prior to
    /// 2026-07-18 this test used `file_by_filename` as the "unsupported"
    /// example -- that request kind is now implemented, see
    /// `reflection_file_by_filename_*` below, so this test moved to a
    /// genuinely still-unsupported oneof field.)
    #[tokio::test]
    async fn reflection_returns_unimplemented_for_unsupported_request_kind() {
        use http_body_util::{BodyExt, Full};
        use hyper_util::client::legacy::connect::HttpConnector;
        use hyper_util::client::legacy::Client;
        use hyper_util::rt::TokioExecutor;

        let (addr, _handle) = serve_grpc("127.0.0.1:0".parse().unwrap()).await.expect("bind grpc port");

        let mut connector = HttpConnector::new();
        connector.enforce_http(false);
        let client: Client<_, Full<Bytes>> =
            Client::builder(TokioExecutor::new()).http2_only(true).build(connector);

        // ServerReflectionRequest { file_containing_extension: ... } -> field 5.
        let mut request_message = vec![0x2a, 7]; // tag(field 5, wire 2), len 7
        request_message.extend_from_slice(b"x.proto");
        let request_body = encode_grpc_frame(&request_message);

        let req = HyperRequest::builder()
            .method("POST")
            .uri(format!(
                "http://{addr}/grpc.reflection.v1.ServerReflection/ServerReflectionInfo"
            ))
            .header("content-type", "application/grpc+proto")
            .body(Full::new(request_body))
            .unwrap();

        let resp = client.request(req).await.expect("HTTP/2 request should succeed");
        let trailers = resp.collect().await.unwrap().trailers().cloned().unwrap_or_default();
        assert_eq!(trailers.get("grpc-status").and_then(|v| v.to_str().ok()), Some("12")); // UNIMPLEMENTED
    }

    /// End-to-end: `file_by_filename` for a filename this server actually
    /// serves (`grpc/health/v1/health.proto`) returns `OK` with a
    /// `file_descriptor_response` carrying the same descriptor bytes as
    /// `file_containing_symbol` -- both are just different lookup keys
    /// (filename vs. symbol name) into the same descriptor.
    #[tokio::test]
    async fn reflection_file_by_filename_returns_the_real_descriptor_over_real_http2() {
        use http_body_util::{BodyExt, Full};
        use hyper_util::client::legacy::connect::HttpConnector;
        use hyper_util::client::legacy::Client;
        use hyper_util::rt::TokioExecutor;

        let (addr, _handle) = serve_grpc("127.0.0.1:0".parse().unwrap()).await.expect("bind grpc port");

        let mut connector = HttpConnector::new();
        connector.enforce_http(false);
        let client: Client<_, Full<Bytes>> =
            Client::builder(TokioExecutor::new()).http2_only(true).build(connector);

        // ServerReflectionRequest { file_by_filename: "grpc/health/v1/health.proto" } -> field 3.
        let filename = "grpc/health/v1/health.proto";
        let mut request_message = vec![0x1a, filename.len() as u8];
        request_message.extend_from_slice(filename.as_bytes());
        let request_body = encode_grpc_frame(&request_message);

        let req = HyperRequest::builder()
            .method("POST")
            .uri(format!(
                "http://{addr}/grpc.reflection.v1.ServerReflection/ServerReflectionInfo"
            ))
            .header("content-type", "application/grpc+proto")
            .body(Full::new(request_body))
            .unwrap();

        let resp = client.request(req).await.expect("HTTP/2 request should succeed");
        assert_eq!(resp.status(), StatusCode::OK);

        let collected = resp.collect().await.expect("collecting body+trailers should succeed");
        let trailers = collected.trailers().cloned().unwrap_or_default();
        assert_eq!(
            trailers.get("grpc-status").and_then(|v| v.to_str().ok()),
            Some("0"),
            "file_by_filename for a known filename should succeed"
        );

        let body_bytes = collected.to_bytes();
        let message = decode_grpc_frame(&body_bytes)
            .expect("reflection response should be a valid gRPC-framed message");
        let as_str = String::from_utf8_lossy(&message);
        assert!(
            as_str.contains("grpc.health.v1.Health"),
            "file descriptor response should embed the real FileDescriptorProto"
        );
    }

    /// End-to-end: `file_by_filename` for an unknown filename returns
    /// `NOT_FOUND`, mirroring `file_containing_symbol`'s unknown-symbol
    /// behavior.
    #[tokio::test]
    async fn reflection_file_by_filename_returns_not_found_for_unknown_filename() {
        use http_body_util::{BodyExt, Full};
        use hyper_util::client::legacy::connect::HttpConnector;
        use hyper_util::client::legacy::Client;
        use hyper_util::rt::TokioExecutor;

        let (addr, _handle) = serve_grpc("127.0.0.1:0".parse().unwrap()).await.expect("bind grpc port");

        let mut connector = HttpConnector::new();
        connector.enforce_http(false);
        let client: Client<_, Full<Bytes>> =
            Client::builder(TokioExecutor::new()).http2_only(true).build(connector);

        let filename = "no/such/file.proto";
        let mut request_message = vec![0x1a, filename.len() as u8];
        request_message.extend_from_slice(filename.as_bytes());
        let request_body = encode_grpc_frame(&request_message);

        let req = HyperRequest::builder()
            .method("POST")
            .uri(format!(
                "http://{addr}/grpc.reflection.v1.ServerReflection/ServerReflectionInfo"
            ))
            .header("content-type", "application/grpc+proto")
            .body(Full::new(request_body))
            .unwrap();

        let resp = client.request(req).await.expect("HTTP/2 request should succeed");
        let trailers = resp.collect().await.unwrap().trailers().cloned().unwrap_or_default();
        assert_eq!(
            trailers.get("grpc-status").and_then(|v| v.to_str().ok()),
            Some("5"), // NOT_FOUND
            "file_by_filename for an unknown filename should return NOT_FOUND"
        );
    }

    #[test]
    fn extract_file_containing_symbol_detects_field_4() {
        // ServerReflectionRequest { file_containing_symbol: "grpc.health.v1.Health" }
        // -> field 4, tag 0x22, length-delimited string.
        let symbol = "grpc.health.v1.Health";
        let mut bytes = vec![0x22, symbol.len() as u8];
        bytes.extend_from_slice(symbol.as_bytes());
        assert_eq!(extract_file_containing_symbol(&bytes).as_deref(), Some(symbol));
    }

    #[test]
    fn extract_file_containing_symbol_ignores_other_oneof_fields() {
        // ServerReflectionRequest { list_services: "" } -> field 7, no
        // file_containing_symbol present.
        let bytes = [0x3a, 0x00];
        assert_eq!(extract_file_containing_symbol(&bytes), None);
    }

    #[test]
    fn resolve_symbol_file_descriptor_recognizes_health_service_and_its_methods() {
        assert!(resolve_symbol_file_descriptor("grpc.health.v1.Health").is_some());
        assert!(resolve_symbol_file_descriptor("grpc.health.v1.Health.Check").is_some());
        assert!(resolve_symbol_file_descriptor("grpc.health.v1.Health.Watch").is_some());
        assert!(resolve_symbol_file_descriptor("no.such.Service").is_none());
    }

    #[test]
    fn extract_file_by_filename_detects_field_3() {
        // ServerReflectionRequest { file_by_filename: "grpc/health/v1/health.proto" }
        // -> field 3, tag 0x1a, length-delimited string.
        let filename = "grpc/health/v1/health.proto";
        let mut bytes = vec![0x1a, filename.len() as u8];
        bytes.extend_from_slice(filename.as_bytes());
        assert_eq!(extract_file_by_filename(&bytes).as_deref(), Some(filename));
    }

    #[test]
    fn extract_file_by_filename_ignores_other_oneof_fields() {
        // ServerReflectionRequest { file_containing_symbol: "grpc.health.v1.Health" }
        // -> field 4, no file_by_filename present.
        let symbol = "grpc.health.v1.Health";
        let mut bytes = vec![0x22, symbol.len() as u8];
        bytes.extend_from_slice(symbol.as_bytes());
        assert_eq!(extract_file_by_filename(&bytes), None);
    }

    #[test]
    fn resolve_filename_file_descriptor_recognizes_the_health_proto_file() {
        assert!(resolve_filename_file_descriptor("grpc/health/v1/health.proto").is_some());
        assert!(resolve_filename_file_descriptor("no/such/file.proto").is_none());
    }

    #[test]
    fn health_file_descriptor_proto_contains_the_real_service_and_method_names() {
        // Not a full protobuf decode (this module never needs a generic
        // descriptor.proto decoder), but the same "assert real content
        // appears in the encoded bytes" technique used for
        // `encode_list_services_response` above -- proves the hand-rolled
        // FileDescriptorProto actually carries the service/method/message
        // names a reflection client would display, not placeholder bytes.
        let bytes = build_health_file_descriptor_proto();
        let as_str = String::from_utf8_lossy(&bytes);
        assert!(as_str.contains("grpc/health/v1/health.proto"));
        assert!(as_str.contains("grpc.health.v1"));
        assert!(as_str.contains("Health"));
        assert!(as_str.contains("Check"));
        assert!(as_str.contains("Watch"));
        assert!(as_str.contains("HealthCheckRequest"));
        assert!(as_str.contains("HealthCheckResponse"));
    }

    /// End-to-end: `file_containing_symbol` for a symbol this server
    /// recognizes (`grpc.health.v1.Health`) returns a
    /// `file_descriptor_response` whose embedded `FileDescriptorProto`
    /// bytes contain the real service/method names -- proves
    /// `grpcurl <addr> describe grpc.health.v1.Health` would get a
    /// genuinely useful answer, not a stub.
    #[tokio::test]
    async fn reflection_file_containing_symbol_returns_the_real_descriptor_over_real_http2() {
        use http_body_util::{BodyExt, Full};
        use hyper_util::client::legacy::connect::HttpConnector;
        use hyper_util::client::legacy::Client;
        use hyper_util::rt::TokioExecutor;

        let (addr, _handle) = serve_grpc("127.0.0.1:0".parse().unwrap()).await.expect("bind grpc port");

        let mut connector = HttpConnector::new();
        connector.enforce_http(false);
        let client: Client<_, Full<Bytes>> =
            Client::builder(TokioExecutor::new()).http2_only(true).build(connector);

        // ServerReflectionRequest { file_containing_symbol: "grpc.health.v1.Health" } -> field 4.
        let symbol = "grpc.health.v1.Health";
        let mut request_message = vec![0x22, symbol.len() as u8];
        request_message.extend_from_slice(symbol.as_bytes());
        let request_body = encode_grpc_frame(&request_message);

        let req = HyperRequest::builder()
            .method("POST")
            .uri(format!(
                "http://{addr}/grpc.reflection.v1.ServerReflection/ServerReflectionInfo"
            ))
            .header("content-type", "application/grpc+proto")
            .body(Full::new(request_body))
            .unwrap();

        let resp = client.request(req).await.expect("HTTP/2 request should succeed");
        let collected = resp.collect().await.expect("collecting body+trailers should succeed");
        let trailers = collected.trailers().cloned().unwrap_or_default();
        assert_eq!(
            trailers.get("grpc-status").and_then(|v| v.to_str().ok()),
            Some("0"),
            "file_containing_symbol for a known symbol should succeed"
        );

        let body_bytes = collected.to_bytes();
        let message = decode_grpc_frame(&body_bytes)
            .expect("reflection response should be a valid gRPC-framed message");
        let as_str = String::from_utf8_lossy(&message);
        assert!(as_str.contains("Health"));
        assert!(as_str.contains("Check"));
        assert!(as_str.contains("Watch"));
    }

    /// End-to-end: `file_containing_symbol` for a symbol this server does
    /// NOT implement must return `NOT_FOUND`, matching how `Check`/`Watch`
    /// already treat an unknown health-check service name -- a reflection
    /// client should never be told a nonexistent service's schema exists.
    #[tokio::test]
    async fn reflection_file_containing_symbol_returns_not_found_for_unknown_symbol() {
        use http_body_util::Full;
        use hyper_util::client::legacy::connect::HttpConnector;
        use hyper_util::client::legacy::Client;
        use hyper_util::rt::TokioExecutor;

        let (addr, _handle) = serve_grpc("127.0.0.1:0".parse().unwrap()).await.expect("bind grpc port");

        let mut connector = HttpConnector::new();
        connector.enforce_http(false);
        let client: Client<_, Full<Bytes>> =
            Client::builder(TokioExecutor::new()).http2_only(true).build(connector);

        let symbol = "no.such.Service";
        let mut request_message = vec![0x22, symbol.len() as u8];
        request_message.extend_from_slice(symbol.as_bytes());
        let request_body = encode_grpc_frame(&request_message);

        let req = HyperRequest::builder()
            .method("POST")
            .uri(format!(
                "http://{addr}/grpc.reflection.v1.ServerReflection/ServerReflectionInfo"
            ))
            .header("content-type", "application/grpc+proto")
            .body(Full::new(request_body))
            .unwrap();

        let resp = client.request(req).await.expect("HTTP/2 request should succeed");
        let trailers = resp.collect().await.unwrap().trailers().cloned().unwrap_or_default();
        assert_eq!(trailers.get("grpc-status").and_then(|v| v.to_str().ok()), Some("5")); // NOT_FOUND
    }

    /// End-to-end test of `check_remote_health` -- the "Cosmo Connect"
    /// gRPC client function -- calling this module's *own* `serve_grpc`
    /// server as if it were an arbitrary external gRPC service. Proves
    /// the client codec (request encoding, response decoding, trailer
    /// parsing) works over a real HTTP/2 connection, not just against
    /// itself in memory.
    #[tokio::test]
    async fn check_remote_health_reports_serving_for_known_service() {
        let (addr, _handle) = serve_grpc("127.0.0.1:0".parse().unwrap()).await.expect("bind grpc port");

        let status = check_remote_health(&addr.to_string(), "")
            .await
            .expect("health check against our own server should succeed");
        assert_eq!(status, ServingStatus::Serving);
    }

    #[tokio::test]
    async fn check_remote_health_errors_for_unknown_service() {
        let (addr, _handle) = serve_grpc("127.0.0.1:0".parse().unwrap()).await.expect("bind grpc port");

        let result = check_remote_health(&addr.to_string(), "nonexistent.Service").await;
        assert!(result.is_err(), "checking an unknown service name should surface as an error, not SERVING");
    }

    #[tokio::test]
    async fn check_remote_health_errors_for_unreachable_address() {
        // Nothing listening on this port -- the connection itself should
        // fail cleanly (an Err, not a panic or hang).
        let result = check_remote_health("127.0.0.1:1", "").await;
        assert!(result.is_err());
    }
}
