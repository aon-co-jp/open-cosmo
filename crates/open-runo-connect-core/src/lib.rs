//! gRPC-web-Connect の中核(HTTPスタックに依存しない純粋な部分。wasm32でもビルドできる)。
//!
//! 1つのパス・1つのサービス定義で、次の**5つの通信方式**を同じように受ける/話すための部品:
//!
//! | 方式 | Content-Type | 向き |
//! |---|---|---|
//! | gRPC | `application/grpc[+proto]` | サーバー間・CLI(HTTP/2のトレーラー) |
//! | gRPC-web | `application/grpc-web[+proto]` | ブラウザ(トレーラーを本文の末尾に置く) |
//! | gRPC-web-text | `application/grpc-web-text[+proto]` | 古いブラウザ/XHR(各フレームをbase64) |
//! | Connect(単項) | `application/proto` / `application/json` | ブラウザ・curl(素の本文、エラーはHTTPステータス) |
//! | Connect(ストリーム) | `application/connect+proto` / `application/connect+json` | ブラウザ(エンベロープ+終端フレーム) |
//!
//! **ハイブリッドの狙い**: Connectの「素のHTTPとして読み書きできる」(curl・開発者ツール・プロキシに
//! 優しい)と、gRPC-webの「gRPCのコード体系・トレーラー・既存クライアント資産」を両取りする。
//! エラー応答はConnect形式(HTTPステータス+JSON)に**gRPCの`grpc-status`/`grpc-message`ヘッダも**
//! 付けるので、どちらの流儀のツールからも読める。

use base64::Engine as _;

pub use base64;
pub use serde_json;

/// エラーJSONなどを読む(読めなければ`None`)。
pub fn parse_json(bytes: &[u8]) -> Option<serde_json::Value> {
    serde_json::from_slice(bytes).ok()
}

// ── gRPCステータスコード ─────────────────────────────────────────────────

/// gRPCのステータスコード(`i32`)と、HTTP・Connectとの対応。
pub mod code {
    pub const OK: i32 = 0;
    pub const CANCELLED: i32 = 1;
    pub const UNKNOWN: i32 = 2;
    pub const INVALID_ARGUMENT: i32 = 3;
    pub const DEADLINE_EXCEEDED: i32 = 4;
    pub const NOT_FOUND: i32 = 5;
    pub const ALREADY_EXISTS: i32 = 6;
    pub const PERMISSION_DENIED: i32 = 7;
    pub const RESOURCE_EXHAUSTED: i32 = 8;
    pub const FAILED_PRECONDITION: i32 = 9;
    pub const ABORTED: i32 = 10;
    pub const OUT_OF_RANGE: i32 = 11;
    pub const UNIMPLEMENTED: i32 = 12;
    pub const INTERNAL: i32 = 13;
    pub const UNAVAILABLE: i32 = 14;
    pub const DATA_LOSS: i32 = 15;
    pub const UNAUTHENTICATED: i32 = 16;

    /// Connectのコード名(`not_found`など。エラーJSONの`code`)。
    pub fn connect_name(code: i32) -> &'static str {
        match code {
            1 => "canceled",
            3 => "invalid_argument",
            4 => "deadline_exceeded",
            5 => "not_found",
            6 => "already_exists",
            7 => "permission_denied",
            8 => "resource_exhausted",
            9 => "failed_precondition",
            10 => "aborted",
            11 => "out_of_range",
            12 => "unimplemented",
            13 => "internal",
            14 => "unavailable",
            15 => "data_loss",
            16 => "unauthenticated",
            _ => "unknown",
        }
    }

    /// gRPCコード → Connectが定めるHTTPステータス。
    pub fn http_status(code: i32) -> u16 {
        match code {
            1 => 499,
            3 | 11 => 400,
            4 => 504,
            5 => 404,
            6 | 10 => 409,
            7 => 403,
            8 => 429,
            9 => 400,
            12 => 501,
            14 => 503,
            16 => 401,
            _ => 500,
        }
    }

    /// HTTPステータス → gRPCコード(アプリの失敗`AdminError`などを写すときに使う)。
    pub fn from_http(status: u16) -> i32 {
        match status {
            400 | 422 => INVALID_ARGUMENT,
            401 => UNAUTHENTICATED,
            403 => PERMISSION_DENIED,
            404 => NOT_FOUND,
            409 => ALREADY_EXISTS,
            429 => RESOURCE_EXHAUSTED,
            499 => CANCELLED,
            501 => UNIMPLEMENTED,
            503 => UNAVAILABLE,
            504 => DEADLINE_EXCEEDED,
            500 => INTERNAL,
            _ => UNKNOWN,
        }
    }

    /// `UNAUTHENTICATED`のような大文字のコード名(CLI・画面の表示用)。
    pub fn name(code: i32) -> &'static str {
        match code {
            0 => "OK",
            1 => "CANCELLED",
            3 => "INVALID_ARGUMENT",
            4 => "DEADLINE_EXCEEDED",
            5 => "NOT_FOUND",
            6 => "ALREADY_EXISTS",
            7 => "PERMISSION_DENIED",
            8 => "RESOURCE_EXHAUSTED",
            9 => "FAILED_PRECONDITION",
            10 => "ABORTED",
            11 => "OUT_OF_RANGE",
            12 => "UNIMPLEMENTED",
            13 => "INTERNAL",
            14 => "UNAVAILABLE",
            15 => "DATA_LOSS",
            16 => "UNAUTHENTICATED",
            _ => "UNKNOWN",
        }
    }
}

/// 失敗(gRPCコード+メッセージ)。サーバー実装・クライアントで共通。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub code: i32,
    pub message: String,
}

impl Status {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// Connectのエラー本文(`{"code":"not_found","message":"..."}`)。
    pub fn connect_json(&self) -> serde_json::Value {
        serde_json::json!({ "code": code::connect_name(self.code), "message": self.message })
    }
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} ({}): {}",
            code::name(self.code),
            self.code,
            self.message
        )
    }
}

impl std::error::Error for Status {}

// ── プロトコルの判別 ─────────────────────────────────────────────────────

/// 本文のコーデック。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    Proto,
    Json,
}

/// 通信方式(Content-Typeから決まる)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// 素のgRPC(HTTP/2トレーラー)。
    Grpc,
    /// gRPC-web(バイナリ、トレーラーは本文の末尾フレーム)。
    GrpcWeb,
    /// gRPC-web-text(各フレームをbase64)。
    GrpcWebText,
    /// Connectの単項(素の本文)。
    ConnectUnary(Codec),
    /// Connectのストリーミング(エンベロープ)。
    ConnectStream(Codec),
}

impl Protocol {
    /// `Content-Type`ヘッダ値から方式を判別する。知らない型なら`None`。
    pub fn from_content_type(ct: &str) -> Option<Protocol> {
        let base = ct
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        match base.as_str() {
            "application/grpc" | "application/grpc+proto" => Some(Protocol::Grpc),
            "application/grpc-web" | "application/grpc-web+proto" => Some(Protocol::GrpcWeb),
            "application/grpc-web-text" | "application/grpc-web-text+proto" => {
                Some(Protocol::GrpcWebText)
            }
            "application/proto" => Some(Protocol::ConnectUnary(Codec::Proto)),
            "application/json" => Some(Protocol::ConnectUnary(Codec::Json)),
            "application/connect+proto" => Some(Protocol::ConnectStream(Codec::Proto)),
            "application/connect+json" => Some(Protocol::ConnectStream(Codec::Json)),
            _ => None,
        }
    }

    /// 応答の`Content-Type`。
    pub fn response_content_type(self) -> &'static str {
        match self {
            Protocol::Grpc => "application/grpc+proto",
            Protocol::GrpcWeb => "application/grpc-web+proto",
            Protocol::GrpcWebText => "application/grpc-web-text+proto",
            Protocol::ConnectUnary(Codec::Proto) => "application/proto",
            Protocol::ConnectUnary(Codec::Json) => "application/json",
            Protocol::ConnectStream(Codec::Proto) => "application/connect+proto",
            Protocol::ConnectStream(Codec::Json) => "application/connect+json",
        }
    }

    pub fn is_connect(self) -> bool {
        matches!(self, Protocol::ConnectUnary(_) | Protocol::ConnectStream(_))
    }
}

// ── フレーミング ─────────────────────────────────────────────────────────

/// 圧縮なしのメッセージフレーム(1バイトのフラグ + 4バイトBEの長さ + 本体)。
/// gRPC・gRPC-web・Connectのストリーミングで共通。
pub fn encode_frame(message: &[u8]) -> Vec<u8> {
    encode_frame_with_flags(0, message)
}

/// フラグ付きのフレーム(gRPC-webのトレーラー`0x80`、Connectの終端`0x02`など)。
pub fn encode_frame_with_flags(flags: u8, message: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + message.len());
    out.push(flags);
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
    encode_frame_with_flags(0x80, body.as_bytes())
}

/// gRPC-webのトレーラー本体から`(grpc-status, grpc-message)`を読む。
pub fn parse_web_trailers(body: &[u8]) -> (i32, String) {
    let text = String::from_utf8_lossy(body);
    let mut status = code::UNKNOWN;
    let mut message = String::new();
    for line in text.split("\r\n") {
        if let Some((k, v)) = line.split_once(':') {
            match k.trim().to_ascii_lowercase().as_str() {
                "grpc-status" => status = v.trim().parse().unwrap_or(code::UNKNOWN),
                "grpc-message" => message = percent_decode(v.trim()),
                _ => {}
            }
        }
    }
    (status, message)
}

/// Connectのストリーム終端フレーム(フラグ`0x02`、本体はJSON)。`error`があれば失敗で終わる。
pub fn encode_connect_end(error: Option<&Status>) -> Vec<u8> {
    let body = match error {
        Some(e) => serde_json::json!({ "error": e.connect_json() }),
        None => serde_json::json!({}),
    };
    encode_frame_with_flags(0x02, body.to_string().as_bytes())
}

/// Connectの終端フレーム本体を読む。正常なら`None`、失敗なら`Some(Status)`。
pub fn parse_connect_end(body: &[u8]) -> Option<Status> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    let e = v.get("error")?;
    let name = e.get("code").and_then(|c| c.as_str()).unwrap_or("unknown");
    let code = (0..=16)
        .find(|c| code::connect_name(*c) == name)
        .unwrap_or(code::UNKNOWN);
    Some(Status::new(
        code,
        e.get("message").and_then(|m| m.as_str()).unwrap_or(""),
    ))
}

/// `grpc-message`用のパーセントエンコード(印字可能ASCII以外と`%`を`%XX`にする)。
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

// ── gRPC-web-text(base64) ───────────────────────────────────────────────

/// base64(パディング付き)。`grpc-web-text`ではフレームごとに独立してエンコードする
/// (仕様上、クライアントはパディング付きのbase64チャンクの連結を受け取れる)。
pub fn b64_encode(bytes: &[u8]) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .encode(bytes)
        .into_bytes()
}

/// `grpc-web-text`の本文(パディング付きチャンクの連結でもよい)を元のバイト列へ戻す。
pub fn decode_text_body(body: &[u8]) -> Option<Vec<u8>> {
    let text: Vec<u8> = body
        .iter()
        .copied()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    let mut out = Vec::new();
    let mut rest: &[u8] = &text;
    while !rest.is_empty() {
        // 次のパディング(`=`)の並びの終わりまでを1チャンクとして読む。
        let end = match rest.iter().position(|&b| b == b'=') {
            Some(i) => {
                let mut j = i;
                while j < rest.len() && rest[j] == b'=' {
                    j += 1;
                }
                j
            }
            None => rest.len(),
        };
        out.extend(
            base64::engine::general_purpose::STANDARD
                .decode(&rest[..end])
                .ok()?,
        );
        rest = &rest[end..];
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_types_map_to_the_five_protocols() {
        use Protocol::*;
        let p = Protocol::from_content_type;
        assert_eq!(p("application/grpc"), Some(Grpc));
        assert_eq!(p("application/grpc+proto"), Some(Grpc));
        assert_eq!(p("application/grpc-web+proto"), Some(GrpcWeb));
        assert_eq!(p("application/grpc-web-text"), Some(GrpcWebText));
        assert_eq!(p("application/proto"), Some(ConnectUnary(Codec::Proto)));
        assert_eq!(
            p("application/json; charset=utf-8"),
            Some(ConnectUnary(Codec::Json))
        );
        assert_eq!(
            p("application/connect+json"),
            Some(ConnectStream(Codec::Json))
        );
        assert_eq!(p("text/plain"), None);
        // 応答の型は要求の型に対応する。
        assert_eq!(
            GrpcWebText.response_content_type(),
            "application/grpc-web-text+proto"
        );
        assert!(ConnectStream(Codec::Proto).is_connect() && !Grpc.is_connect());
    }

    #[test]
    fn status_mapping_round_trips() {
        for c in [3, 5, 6, 7, 8, 12, 14, 16] {
            assert_eq!(code::from_http(code::http_status(c)), c, "{c}");
        }
        assert_eq!(code::connect_name(code::NOT_FOUND), "not_found");
        assert_eq!(code::http_status(code::UNAUTHENTICATED), 401);
        assert_eq!(code::from_http(422), code::INVALID_ARGUMENT);
    }

    #[test]
    fn frames_trailers_and_end_frames_round_trip() {
        let f = encode_frame(b"hello");
        assert_eq!(split_frame(&f), Some((0, &b"hello"[..], &b""[..])));
        assert!(split_frame(&f[..4]).is_none());

        let t = encode_web_trailers(5, "見つかりません: a%b");
        let (flag, body, _) = split_frame(&t).unwrap();
        assert_eq!(flag, 0x80);
        assert_eq!(
            parse_web_trailers(body),
            (5, "見つかりません: a%b".to_string())
        );

        let end = encode_connect_end(Some(&Status::new(code::NOT_FOUND, "x")));
        let (flag, body, _) = split_frame(&end).unwrap();
        assert_eq!(flag, 0x02);
        assert_eq!(
            parse_connect_end(body),
            Some(Status::new(code::NOT_FOUND, "x"))
        );
        let ok = encode_connect_end(None);
        assert_eq!(parse_connect_end(split_frame(&ok).unwrap().1), None);
    }

    #[test]
    fn text_body_decoder_accepts_concatenated_padded_chunks() {
        let (a, b) = (b64_encode(b"hello"), b64_encode(b"world!!"));
        let joined = [a.clone(), b.clone()].concat();
        assert_eq!(decode_text_body(&joined).unwrap(), b"helloworld!!");
        let spaced = [a, b"\n".to_vec(), b].concat();
        assert_eq!(decode_text_body(&spaced).unwrap(), b"helloworld!!");
        assert!(decode_text_body(b"@@@@").is_none());
    }
}
