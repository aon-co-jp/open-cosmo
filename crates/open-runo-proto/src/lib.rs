//! open-runo 管理用 gRPC の共有定義。
//!
//! - [`v1`]: `admin.proto` から生成した prost のメッセージ型(サーバー・CLI・管理画面WASMで共通)。
//! - [`FILE_DESCRIPTOR_SET`]: リフレクション用の記述子セット(`admin.proto`と依存の
//!   `google/protobuf/struct.proto`)。
//! - JSON ⇔ `google.protobuf.Struct` 変換: REST時代と同じJSON形の応答をStructで運ぶ。
//! - gRPC / gRPC-web のフレーミング(長さ前置メッセージ、gRPC-webのトレーラーフレーム)。
//!   サーバー・クライアント・WASMが同じ実装を使う。

// このクレートは生成コードを含むため、missing_debug_implementations は生成側に委ねる。
#![allow(missing_docs)]

pub use prost;
pub use prost_types;

#[cfg(feature = "client")]
pub mod client;

/// 生成された型。`open_runo.admin.v1`。
pub mod v1 {
    include!(concat!(env!("OUT_DIR"), "/open_runo.admin.v1.rs"));
}

/// `admin.proto` の `FileDescriptorSet`(エンコード済み)。
pub const FILE_DESCRIPTOR_SET: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/admin_fds.bin"));

/// `admin.proto`の本文(CLIの`proto`コマンドや文書用。定義の唯一の正)。
pub const ADMIN_PROTO: &str = include_str!("../proto/open_runo/admin/v1/admin.proto");

/// 管理サービスの完全修飾名。
pub const ADMIN_SERVICE: &str = "open_runo.admin.v1.Admin";

/// `/open_runo.admin.v1.Admin/<method>` のパス。
pub fn method_path(method: &str) -> String {
    format!("/{ADMIN_SERVICE}/{method}")
}

// ── JSON ⇔ Struct ─────────────────────────────────────────────────────────

use prost_types::value::Kind;
use serde_json::Value as Json;

/// `serde_json::Value` → `google.protobuf.Value`。
pub fn json_to_value(v: &Json) -> prost_types::Value {
    let kind = match v {
        Json::Null => Kind::NullValue(0),
        Json::Bool(b) => Kind::BoolValue(*b),
        Json::Number(n) => Kind::NumberValue(n.as_f64().unwrap_or(0.0)),
        Json::String(s) => Kind::StringValue(s.clone()),
        Json::Array(a) => Kind::ListValue(prost_types::ListValue {
            values: a.iter().map(json_to_value).collect(),
        }),
        Json::Object(o) => Kind::StructValue(prost_types::Struct {
            fields: o
                .iter()
                .map(|(k, v)| (k.clone(), json_to_value(v)))
                .collect(),
        }),
    };
    prost_types::Value { kind: Some(kind) }
}

/// `google.protobuf.Value` → `serde_json::Value`。整数で表せる数値は整数にする
/// (Structの数値はdoubleなので、`records`のような件数が`1.0`にならないようにする)。
pub fn value_to_json(v: &prost_types::Value) -> Json {
    match &v.kind {
        None | Some(Kind::NullValue(_)) => Json::Null,
        Some(Kind::BoolValue(b)) => Json::Bool(*b),
        Some(Kind::NumberValue(n)) => {
            if n.fract() == 0.0 && n.abs() < 9.0e15 {
                Json::from(*n as i64)
            } else {
                serde_json::Number::from_f64(*n)
                    .map(Json::Number)
                    .unwrap_or(Json::Null)
            }
        },
        Some(Kind::StringValue(s)) => Json::String(s.clone()),
        Some(Kind::ListValue(l)) => Json::Array(l.values.iter().map(value_to_json).collect()),
        Some(Kind::StructValue(s)) => struct_to_json(s),
    }
}

/// JSONオブジェクト → `google.protobuf.Struct`(オブジェクト以外は空Struct)。
pub fn json_to_struct(v: &Json) -> prost_types::Struct {
    match v {
        Json::Object(o) => prost_types::Struct {
            fields: o
                .iter()
                .map(|(k, v)| (k.clone(), json_to_value(v)))
                .collect(),
        },
        _ => prost_types::Struct::default(),
    }
}

/// `google.protobuf.Struct` → JSONオブジェクト。
pub fn struct_to_json(s: &prost_types::Struct) -> Json {
    Json::Object(
        s.fields
            .iter()
            .map(|(k, v)| (k.clone(), value_to_json(v)))
            .collect(),
    )
}

// ── gRPC / gRPC-web フレーミング ──────────────────────────────────────────

/// 圧縮なしのgRPCメッセージフレーム(1バイトのフラグ + 4バイトBEの長さ + 本体)を作る。
pub fn encode_frame(message: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + message.len());
    out.push(0);
    out.extend_from_slice(&(message.len() as u32).to_be_bytes());
    out.extend_from_slice(message);
    out
}

/// 1フレームを取り出す。`(フラグ, 本体, 残り)`。足りなければ`None`。
pub fn split_frame(buf: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    if buf.len() < 5 {
        return None;
    }
    let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
    let body = buf.get(5..5 + len)?;
    Some((buf[0], body, &buf[5 + len..]))
}

/// gRPC-webのトレーラーフレーム(フラグ`0x80`、本体は`key: value\r\n`の並び)。
pub fn encode_web_trailers(status: i32, message: &str) -> Vec<u8> {
    let body = format!(
        "grpc-status: {status}\r\ngrpc-message: {}\r\n",
        percent_encode(message)
    );
    let mut out = Vec::with_capacity(5 + body.len());
    out.push(0x80);
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(body.as_bytes());
    out
}

/// gRPC-webのトレーラー本体から`(grpc-status, grpc-message)`を読む。
pub fn parse_web_trailers(body: &[u8]) -> (i32, String) {
    let text = String::from_utf8_lossy(body);
    let mut status = 2;
    let mut message = String::new();
    for line in text.split("\r\n") {
        if let Some((k, v)) = line.split_once(':') {
            match k.trim().to_ascii_lowercase().as_str() {
                "grpc-status" => status = v.trim().parse().unwrap_or(2),
                "grpc-message" => message = percent_decode(v.trim()),
                _ => {},
            }
        }
    }
    (status, message)
}

/// `grpc-message`用のパーセントエンコード(仕様: 印字可能ASCII以外と`%`を`%XX`にする)。
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if (0x20..=0x7e).contains(&b) && b != b'%' {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// [`percent_encode`]の逆。
pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(byte) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// gRPCステータスコード(使うものだけ)。
pub mod code {
    pub const OK: i32 = 0;
    pub const UNKNOWN: i32 = 2;
    pub const INVALID_ARGUMENT: i32 = 3;
    pub const NOT_FOUND: i32 = 5;
    pub const ALREADY_EXISTS: i32 = 6;
    pub const PERMISSION_DENIED: i32 = 7;
    pub const RESOURCE_EXHAUSTED: i32 = 8;
    pub const UNIMPLEMENTED: i32 = 12;
    pub const INTERNAL: i32 = 13;
    pub const UNAUTHENTICATED: i32 = 16;

    /// 旧RESTのHTTPステータス → gRPCコード(`AdminError.status`の変換)。
    pub fn from_http(status: u16) -> i32 {
        match status {
            400 | 422 => INVALID_ARGUMENT,
            401 => UNAUTHENTICATED,
            403 => PERMISSION_DENIED,
            404 => NOT_FOUND,
            409 => ALREADY_EXISTS,
            429 => RESOURCE_EXHAUSTED,
            501 => UNIMPLEMENTED,
            500 => INTERNAL,
            _ => UNKNOWN,
        }
    }

    /// コード名(CLI/画面の表示用)。
    pub fn name(code: i32) -> &'static str {
        match code {
            0 => "OK",
            3 => "INVALID_ARGUMENT",
            5 => "NOT_FOUND",
            6 => "ALREADY_EXISTS",
            7 => "PERMISSION_DENIED",
            8 => "RESOURCE_EXHAUSTED",
            12 => "UNIMPLEMENTED",
            13 => "INTERNAL",
            16 => "UNAUTHENTICATED",
            _ => "UNKNOWN",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_struct_round_trip_keeps_integers() {
        let j =
            serde_json::json!({ "records": 17, "ok": true, "tags": ["a", "b"], "n": { "x": 1.5 } });
        let back = struct_to_json(&json_to_struct(&j));
        assert_eq!(back, j);
    }

    #[test]
    fn frames_round_trip() {
        let f = encode_frame(b"hello");
        let (flag, body, rest) = split_frame(&f).unwrap();
        assert_eq!((flag, body, rest.len()), (0, &b"hello"[..], 0));
        assert!(split_frame(&f[..4]).is_none());
    }

    #[test]
    fn web_trailers_round_trip_with_non_ascii_message() {
        let t = encode_web_trailers(5, "見つかりません: a%b");
        let (flag, body, _) = split_frame(&t).unwrap();
        assert_eq!(flag, 0x80);
        assert_eq!(
            parse_web_trailers(body),
            (5, "見つかりません: a%b".to_string())
        );
    }

    #[test]
    fn descriptor_set_contains_the_admin_service() {
        use prost::Message;
        let fds = prost_types::FileDescriptorSet::decode(FILE_DESCRIPTOR_SET).unwrap();
        let admin = fds
            .file
            .iter()
            .find(|f| f.name() == "open_runo/admin/v1/admin.proto")
            .expect("admin.proto");
        assert_eq!(admin.service[0].name(), "Admin");
        assert!(admin.service[0].method.len() >= 35);
        assert!(fds
            .file
            .iter()
            .any(|f| f.name() == "google/protobuf/struct.proto"));
    }

    #[test]
    fn messages_encode_and_decode() {
        use prost::Message;
        let m = v1::RegisterSchemaRequest {
            service_name: "users".into(),
            sdl: "type Query { a: Int }".into(),
            stage: Some("staging".into()),
            namespace: None,
        };
        let bytes = m.encode_to_vec();
        assert_eq!(v1::RegisterSchemaRequest::decode(&bytes[..]).unwrap(), m);
    }
}
