//! ストリーミング `multipart/form-data` パーサー(RFC 7578)。
//!
//! `hyper_compat::read_multipart_body` はボディ全体をメモリへ集めてから
//! 解析するため、巨大ファイルのアップロードでメモリが膨らむ(Axumの
//! `multer`のように「フィールドを1つずつ・チャンクごとに」読めない)。
//! 本モジュールはその不足を埋める、依存追加なしの手書き実装である。
//!
//! - ボディは到着したチャンクごとに処理し、メモリ使用量は「境界文字列の
//!   長さ+1チャンク」程度で、ファイルサイズに依存しない。
//! - チャンク境界をまたいで境界文字列が分割されても正しく検出する
//!   (テストで全分割位置を総当たりで確認している)。
//! - 合計サイズ・1フィールドのサイズ・フィールド数・ヘッダ長に上限を持つ。
//!   上限超過は `413`、不正な形式は `400` の応答へ変換できる。
//!
//! 使い方(ハンドラ内):
//! ```ignore
//! let mut mp = MultipartStream::from_request(req, MultipartLimits::default())?;
//! while let Some(field) = mp.next_field().await.map_err(|e| e.into_response())? {
//!     while let Some(chunk) = mp.chunk().await.map_err(|e| e.into_response())? {
//!         // chunk(Bytes)をファイルや後段へ流す
//!     }
//! }
//! ```
//! `next_field()` は読み残した現在のフィールドを自動で読み捨ててから
//! 次へ進む。

#![allow(clippy::result_large_err)]

use crate::hyper_compat::{json_response, Request, Response};
use bytes::{Buf, Bytes, BytesMut};
use futures::{Stream, StreamExt};
use http_body_util::{BodyDataStream, BodyExt};
use hyper::body::Incoming;
use hyper::StatusCode;
use std::fmt;

/// 受け付けるサイズ・件数の上限。既定値は一般的なアップロード向けの
/// 控えめな値で、用途に応じて呼び出し側が明示的に引き上げる。
#[derive(Debug, Clone, Copy)]
pub struct MultipartLimits {
    /// リクエストボディ全体の上限(バイト)。
    pub max_total_bytes: u64,
    /// 1フィールド(ファイル)の本体の上限(バイト)。
    pub max_field_bytes: u64,
    /// フィールド数の上限。
    pub max_fields: usize,
    /// 1フィールド分のヘッダブロックの上限(バイト)。
    pub max_header_bytes: usize,
}

impl Default for MultipartLimits {
    fn default() -> Self {
        Self {
            max_total_bytes: 64 * 1024 * 1024,
            max_field_bytes: 32 * 1024 * 1024,
            max_fields: 64,
            max_header_bytes: 16 * 1024,
        }
    }
}

#[derive(Debug)]
pub enum MultipartError {
    /// `Content-Type` が `multipart/form-data` でない/boundary が無い。
    BadContentType(&'static str),
    /// 形式が壊れている(境界が閉じない・ヘッダが不正など)。
    Malformed(&'static str),
    /// 上限超過。
    TooLarge(&'static str),
    /// 下位のボディ読み取りエラー(切断など)。
    Body(String),
}

impl MultipartError {
    pub fn status(&self) -> StatusCode {
        match self {
            MultipartError::TooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
            _ => StatusCode::BAD_REQUEST,
        }
    }

    pub fn into_response(self) -> Response {
        json_response(
            self.status(),
            &serde_json::json!({ "error": self.to_string() }),
        )
    }
}

impl fmt::Display for MultipartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MultipartError::BadContentType(m) => write!(f, "{m}"),
            MultipartError::Malformed(m) => write!(f, "malformed multipart body: {m}"),
            MultipartError::TooLarge(m) => write!(f, "multipart body too large: {m}"),
            MultipartError::Body(m) => write!(f, "failed to read request body: {m}"),
        }
    }
}

impl std::error::Error for MultipartError {}

/// 1つのフィールドのヘッダ情報(本体は [`MultipartStream::chunk`] で読む)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldInfo {
    pub name: String,
    pub filename: Option<String>,
    pub content_type: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    /// 最初の境界を探している。
    Preamble,
    /// 境界を読み終えた直後(`--`=終端 / CRLF=次のパーツ)。
    Delimited,
    /// パーツ本体を読んでいる。
    Body,
    Done,
}

pub struct MultipartStream<S> {
    stream: S,
    /// `\r\n--boundary`(最初の境界にも一様に当てはまるよう、バッファの
    /// 先頭に仮想的なCRLFを置いている)。
    delimiter: Vec<u8>,
    buf: BytesMut,
    state: State,
    limits: MultipartLimits,
    total: u64,
    field_bytes: u64,
    fields: usize,
    eof: bool,
}

impl<S> fmt::Debug for MultipartStream<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MultipartStream")
            .field("buffered", &self.buf.len())
            .field("total", &self.total)
            .field("fields", &self.fields)
            .finish()
    }
}

impl MultipartStream<BodyDataStream<Incoming>> {
    /// リクエストから構築する。`Content-Type` と boundary を検証し、
    /// 不正なら完成済みの `400` 応答を `Err` で返す。
    pub fn from_request(req: Request, limits: MultipartLimits) -> Result<Self, Response> {
        let content_type = req
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        // Content-Length が分かっていれば、読み始める前に拒否する。
        if let Some(len) = req
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
        {
            if len > limits.max_total_bytes {
                return Err(
                    MultipartError::TooLarge("content-length exceeds limit").into_response()
                );
            }
        }
        let boundary = boundary_from_content_type(&content_type).map_err(|e| e.into_response())?;
        Ok(Self::new(
            req.into_body().into_data_stream(),
            &boundary,
            limits,
        ))
    }
}

impl<S, E> MultipartStream<S>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin,
    E: fmt::Display,
{
    /// 任意のチャンクストリームから構築する(テストや、ボディ以外の
    /// 入力元向け)。`boundary` は `Content-Type` の値(引用符なし)。
    pub fn new(stream: S, boundary: &str, limits: MultipartLimits) -> Self {
        let mut delimiter = Vec::with_capacity(boundary.len() + 4);
        delimiter.extend_from_slice(b"\r\n--");
        delimiter.extend_from_slice(boundary.as_bytes());
        let mut buf = BytesMut::new();
        buf.extend_from_slice(b"\r\n");
        Self {
            stream,
            delimiter,
            buf,
            state: State::Preamble,
            limits,
            total: 0,
            field_bytes: 0,
            fields: 0,
            eof: false,
        }
    }

    /// ストリームから次のチャンクをバッファへ足す。EOFなら `false`。
    async fn fill(&mut self) -> Result<bool, MultipartError> {
        if self.eof {
            return Ok(false);
        }
        match self.stream.next().await {
            Some(Ok(chunk)) => {
                self.total += chunk.len() as u64;
                if self.total > self.limits.max_total_bytes {
                    return Err(MultipartError::TooLarge("total body exceeds limit"));
                }
                self.buf.extend_from_slice(&chunk);
                Ok(true)
            },
            Some(Err(e)) => Err(MultipartError::Body(e.to_string())),
            None => {
                self.eof = true;
                Ok(false)
            },
        }
    }

    /// 次のフィールドへ進む。現在のフィールドの読み残しは読み捨てる。
    /// 終端なら `Ok(None)`。名前の無いパーツは読み捨てて次へ進む。
    pub async fn next_field(&mut self) -> Result<Option<FieldInfo>, MultipartError> {
        loop {
            if self.state == State::Body {
                while self.chunk().await?.is_some() {}
            }
            match self.state {
                State::Done => return Ok(None),
                State::Preamble => {
                    if let Some(idx) = find(&self.buf, &self.delimiter) {
                        self.buf.advance(idx + self.delimiter.len());
                        self.state = State::Delimited;
                    } else {
                        // 境界の一部が末尾に来ている可能性があるぶんだけ残す。
                        let keep = self.delimiter.len() - 1;
                        if self.buf.len() > keep {
                            let drop = self.buf.len() - keep;
                            self.buf.advance(drop);
                        }
                        if !self.fill().await? {
                            return Err(MultipartError::Malformed("boundary not found"));
                        }
                    }
                },
                State::Delimited => {
                    while self.buf.len() < 2 {
                        if !self.fill().await? {
                            return Err(MultipartError::Malformed("unexpected end after boundary"));
                        }
                    }
                    if self.buf.starts_with(b"--") {
                        self.state = State::Done;
                        return Ok(None);
                    }
                    if !self.buf.starts_with(b"\r\n") {
                        return Err(MultipartError::Malformed("expected CRLF after boundary"));
                    }
                    self.buf.advance(2);
                    let info = self.read_headers().await?;
                    self.state = State::Body;
                    self.field_bytes = 0;
                    match info {
                        Some(info) => {
                            self.fields += 1;
                            if self.fields > self.limits.max_fields {
                                return Err(MultipartError::TooLarge("too many fields"));
                            }
                            return Ok(Some(info));
                        },
                        // 名前なし: 本体を読み捨てるため、ループ先頭で
                        // State::Body の読み捨てに入る。
                        None => continue,
                    }
                },
                State::Body => unreachable!("drained above"),
            }
        }
    }

    /// ヘッダブロックを読んで解析する。`name` が無ければ `None`。
    async fn read_headers(&mut self) -> Result<Option<FieldInfo>, MultipartError> {
        // ヘッダが1つも無いパーツは、直後がすぐ空行になる。
        let end = loop {
            if self.buf.starts_with(b"\r\n") {
                self.buf.advance(2);
                return Ok(None);
            }
            if let Some(i) = find(&self.buf, b"\r\n\r\n") {
                break i;
            }
            if self.buf.len() > self.limits.max_header_bytes {
                return Err(MultipartError::TooLarge("part headers too long"));
            }
            if !self.fill().await? {
                return Err(MultipartError::Malformed("unexpected end in part headers"));
            }
        };
        if end > self.limits.max_header_bytes {
            return Err(MultipartError::TooLarge("part headers too long"));
        }
        let header_block = self.buf.split_to(end).freeze();
        self.buf.advance(4);
        let text = String::from_utf8_lossy(&header_block);

        let mut name = None;
        let mut filename = None;
        let mut content_type = None;
        for line in text.split("\r\n") {
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let key = key.trim();
            if key.eq_ignore_ascii_case("content-disposition") {
                for (k, v) in header_params(value) {
                    if k.eq_ignore_ascii_case("name") {
                        name = Some(v);
                    } else if k.eq_ignore_ascii_case("filename") {
                        filename = Some(v);
                    }
                }
            } else if key.eq_ignore_ascii_case("content-type") {
                content_type = Some(value.trim().to_string());
            }
        }
        Ok(name.map(|name| FieldInfo {
            name,
            filename,
            content_type,
        }))
    }

    /// 現在のフィールド本体の次のチャンク。終わりなら `Ok(None)`。
    pub async fn chunk(&mut self) -> Result<Option<Bytes>, MultipartError> {
        loop {
            if self.state != State::Body {
                return Ok(None);
            }
            if let Some(idx) = find(&self.buf, &self.delimiter) {
                let data = self.buf.split_to(idx).freeze();
                self.buf.advance(self.delimiter.len());
                self.state = State::Delimited;
                return self.emit(data);
            }
            // 境界の先頭部分かもしれない末尾は残し、それ以前を渡す。
            let safe = self.buf.len().saturating_sub(self.delimiter.len() - 1);
            if safe > 0 {
                let data = self.buf.split_to(safe).freeze();
                return self.emit(data);
            }
            if !self.fill().await? {
                return Err(MultipartError::Malformed("unexpected end inside a part"));
            }
        }
    }

    fn emit(&mut self, data: Bytes) -> Result<Option<Bytes>, MultipartError> {
        self.field_bytes += data.len() as u64;
        if self.field_bytes > self.limits.max_field_bytes {
            return Err(MultipartError::TooLarge("field exceeds limit"));
        }
        // 空のデータは「終わり」と区別するため返さない
        // (境界の直前でちょうど0バイトが残った場合)。
        Ok(if data.is_empty() { None } else { Some(data) })
    }

    /// 現在のフィールドを丸ごとメモリへ読む(`limit` バイトまで)。
    /// テキスト項目向け。巨大ファイルには `chunk()` を使うこと。
    pub async fn field_bytes(&mut self, limit: usize) -> Result<Vec<u8>, MultipartError> {
        let mut out = Vec::new();
        while let Some(chunk) = self.chunk().await? {
            if out.len() + chunk.len() > limit {
                return Err(MultipartError::TooLarge("field exceeds in-memory limit"));
            }
            out.extend_from_slice(&chunk);
        }
        Ok(out)
    }
}

/// `Content-Type` から boundary を取り出す。
pub fn boundary_from_content_type(content_type: &str) -> Result<String, MultipartError> {
    let lower = content_type.to_ascii_lowercase();
    if !lower.trim_start().starts_with("multipart/form-data") {
        return Err(MultipartError::BadContentType(
            "expected multipart/form-data request body",
        ));
    }
    for (k, v) in header_params(content_type) {
        if k.eq_ignore_ascii_case("boundary") && !v.is_empty() {
            // RFC 2046: 70文字以内。
            if v.len() > 70 {
                return Err(MultipartError::BadContentType(
                    "multipart boundary too long",
                ));
            }
            return Ok(v);
        }
    }
    Err(MultipartError::BadContentType(
        "multipart/form-data content-type missing boundary",
    ))
}

/// `value; key="quoted;value"; key2=token` 形式の補助パラメータを、
/// 引用符内の `;` を壊さずに分解する(先頭の主値は捨てる)。
fn header_params(value: &str) -> Vec<(String, String)> {
    let mut parts: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut escaped = false;
    for c in value.chars() {
        if escaped {
            cur.push(c);
            escaped = false;
        } else if in_quotes && c == '\\' {
            escaped = true;
        } else if c == '"' {
            in_quotes = !in_quotes;
            cur.push(c);
        } else if c == ';' && !in_quotes {
            parts.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    parts.push(cur);
    parts
        .into_iter()
        .skip(1)
        .filter_map(|p| {
            let (k, v) = p.split_once('=')?;
            let v = v.trim();
            let v = v
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .unwrap_or(v);
            Some((k.trim().to_string(), v.to_string()))
        })
        .collect()
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    let first = needle[0];
    let last_start = hay.len() - needle.len();
    let mut i = 0;
    while i <= last_start {
        i += hay[i..=last_start].iter().position(|&b| b == first)?;
        if &hay[i..i + needle.len()] == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;
    use std::convert::Infallible;

    const B: &str = "XyZ-boundary";

    fn sample_body() -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(b"preamble ignored\r\n");
        v.extend_from_slice(format!("--{B}\r\n").as_bytes());
        v.extend_from_slice(
            b"Content-Disposition: form-data; name=\"title\"\r\n\r\nhello world\r\n",
        );
        v.extend_from_slice(format!("--{B}\r\n").as_bytes());
        v.extend_from_slice(
            b"Content-Disposition: form-data; name=\"file\"; filename=\"a;b \\\"c\\\".bin\"\r\nContent-Type: application/octet-stream\r\n\r\n",
        );
        // 本体に「境界に似た」文字列とCRLFを含める。
        v.extend_from_slice(b"line1\r\n--XyZ-bound\r\n--not-a-boundary\r\n\r\nend");
        v.extend_from_slice(format!("\r\n--{B}\r\n").as_bytes());
        v.extend_from_slice(b"Content-Disposition: form-data; name=\"empty\"\r\n\r\n");
        v.extend_from_slice(format!("\r\n--{B}--\r\ntrailing epilogue").as_bytes());
        v
    }

    fn chunked(body: &[u8], size: usize) -> impl Stream<Item = Result<Bytes, Infallible>> + Unpin {
        let chunks: Vec<_> = body
            .chunks(size)
            .map(|c| Ok(Bytes::copy_from_slice(c)))
            .collect();
        stream::iter(chunks)
    }

    async fn collect_all<S, E>(mp: &mut MultipartStream<S>) -> Vec<(FieldInfo, Vec<u8>)>
    where
        S: Stream<Item = Result<Bytes, E>> + Unpin,
        E: fmt::Display,
    {
        let mut out = Vec::new();
        while let Some(info) = mp.next_field().await.unwrap() {
            let data = mp.field_bytes(usize::MAX).await.unwrap();
            out.push((info, data));
        }
        out
    }

    #[tokio::test]
    async fn parses_for_every_chunk_size() {
        let body = sample_body();
        // 1バイトずつからボディ全体まで、あらゆる分割位置で同じ結果になる。
        for size in 1..=body.len() {
            let mut mp = MultipartStream::new(chunked(&body, size), B, MultipartLimits::default());
            let fields = collect_all(&mut mp).await;
            assert_eq!(fields.len(), 3, "chunk size {size}");
            assert_eq!(fields[0].0.name, "title");
            assert_eq!(fields[0].1, b"hello world");
            assert_eq!(fields[1].0.name, "file");
            assert_eq!(fields[1].0.filename.as_deref(), Some("a;b \"c\".bin"));
            assert_eq!(
                fields[1].0.content_type.as_deref(),
                Some("application/octet-stream")
            );
            assert_eq!(
                fields[1].1,
                b"line1\r\n--XyZ-bound\r\n--not-a-boundary\r\n\r\nend"
            );
            assert_eq!(fields[2].0.name, "empty");
            assert!(fields[2].1.is_empty());
        }
    }

    #[tokio::test]
    async fn unread_field_is_skipped_by_next_field() {
        let body = sample_body();
        let mut mp = MultipartStream::new(chunked(&body, 7), B, MultipartLimits::default());
        let first = mp.next_field().await.unwrap().unwrap();
        assert_eq!(first.name, "title");
        // 本体を読まずに次へ。
        let second = mp.next_field().await.unwrap().unwrap();
        assert_eq!(second.name, "file");
    }

    #[tokio::test]
    async fn large_field_streams_in_bounded_memory() {
        let payload = vec![b'x'; 5 * 1024 * 1024];
        let mut body = Vec::new();
        body.extend_from_slice(format!("--{B}\r\n").as_bytes());
        body.extend_from_slice(
            b"Content-Disposition: form-data; name=\"f\"; filename=\"big\"\r\n\r\n",
        );
        body.extend_from_slice(&payload);
        body.extend_from_slice(format!("\r\n--{B}--\r\n").as_bytes());
        let mut mp = MultipartStream::new(chunked(&body, 16 * 1024), B, MultipartLimits::default());
        mp.next_field().await.unwrap().unwrap();
        let mut seen = 0usize;
        let mut max_chunk = 0usize;
        while let Some(c) = mp.chunk().await.unwrap() {
            seen += c.len();
            max_chunk = max_chunk.max(c.len());
        }
        assert_eq!(seen, payload.len());
        // 1回に渡るのは入力チャンク程度で、ファイル全体を溜めない。
        assert!(max_chunk <= 64 * 1024, "max_chunk={max_chunk}");
        assert!(mp.buf.len() < 64 * 1024);
    }

    #[tokio::test]
    async fn field_limit_is_enforced() {
        let body = sample_body();
        let limits = MultipartLimits {
            max_field_bytes: 5,
            ..Default::default()
        };
        let mut mp = MultipartStream::new(chunked(&body, 11), B, limits);
        mp.next_field().await.unwrap().unwrap();
        let err = mp.field_bytes(usize::MAX).await.unwrap_err();
        assert_eq!(err.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn total_limit_and_field_count_are_enforced() {
        let body = sample_body();
        let limits = MultipartLimits {
            max_total_bytes: 50,
            ..Default::default()
        };
        let mut mp = MultipartStream::new(chunked(&body, 10), B, limits);
        let err = loop {
            match mp.next_field().await {
                Ok(Some(_)) => {
                    if let Err(e) = mp.field_bytes(usize::MAX).await {
                        break e;
                    }
                },
                Ok(None) => panic!("should hit the limit"),
                Err(e) => break e,
            }
        };
        assert_eq!(err.status(), StatusCode::PAYLOAD_TOO_LARGE);

        let limits = MultipartLimits {
            max_fields: 2,
            ..Default::default()
        };
        let mut mp = MultipartStream::new(chunked(&body, 10), B, limits);
        let mut last = Ok(None);
        for _ in 0..3 {
            last = mp.next_field().await;
            if last.is_err() {
                break;
            }
        }
        assert_eq!(last.unwrap_err().status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn truncated_body_is_malformed_not_a_hang() {
        let body = sample_body();
        let cut = &body[..body.len() - 40];
        let mut mp = MultipartStream::new(chunked(cut, 9), B, MultipartLimits::default());
        let mut result = Ok(());
        while result.is_ok() {
            match mp.next_field().await {
                Ok(Some(_)) => result = mp.field_bytes(usize::MAX).await.map(|_| ()),
                Ok(None) => break,
                Err(e) => result = Err(e),
            }
        }
        assert_eq!(result.unwrap_err().status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn content_type_boundary_parsing() {
        assert_eq!(
            boundary_from_content_type("multipart/form-data; boundary=abc").unwrap(),
            "abc"
        );
        assert_eq!(
            boundary_from_content_type("Multipart/Form-Data; charset=utf-8; BOUNDARY=\"a;b\"")
                .unwrap(),
            "a;b"
        );
        assert!(boundary_from_content_type("application/json").is_err());
        assert!(boundary_from_content_type("multipart/form-data").is_err());
    }

    /// 実TCPで、`reqwest`のmultipart送信を本パーサーで受ける通し試験。
    #[tokio::test]
    async fn end_to_end_over_real_tcp() {
        use crate::hyper_compat::{serve, Router};
        use hyper::Method;
        use std::sync::Arc;

        let handler: crate::hyper_compat::Handler = Arc::new(|req, _p| {
            Box::pin(async move {
                let mut mp = match MultipartStream::from_request(req, MultipartLimits::default()) {
                    Ok(mp) => mp,
                    Err(resp) => return resp,
                };
                let mut summary = Vec::new();
                loop {
                    match mp.next_field().await {
                        Ok(Some(info)) => {
                            let mut n = 0usize;
                            loop {
                                match mp.chunk().await {
                                    Ok(Some(c)) => n += c.len(),
                                    Ok(None) => break,
                                    Err(e) => return e.into_response(),
                                }
                            }
                            summary.push(serde_json::json!({ "name": info.name, "filename": info.filename, "len": n }));
                        },
                        Ok(None) => break,
                        Err(e) => return e.into_response(),
                    }
                }
                json_response(StatusCode::OK, &summary)
            })
        });
        let router = Router::new().route(Method::POST, "/upload", handler);
        let (addr, _h) = serve(router, "127.0.0.1:0".parse().unwrap()).await.unwrap();

        let form = reqwest::multipart::Form::new().text("note", "hi").part(
            "file",
            reqwest::multipart::Part::bytes(vec![7u8; 300_000]).file_name("data.bin"),
        );
        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/upload"))
            .multipart(form)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let v: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(v[0]["name"], "note");
        assert_eq!(v[0]["len"], 2);
        assert_eq!(v[1]["filename"], "data.bin");
        assert_eq!(v[1]["len"], 300_000);

        // 上限超過は413。
        let big = reqwest::multipart::Form::new().part(
            "file",
            reqwest::multipart::Part::bytes(vec![1u8; 70 * 1024 * 1024]).file_name("big"),
        );
        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/upload"))
            .multipart(big)
            .send()
            .await;
        // サーバーが413を返して接続を閉じるため、応答が読めるか、送信中に切られる。
        if let Ok(r) = resp {
            assert_eq!(r.status(), 413);
        }
    }
}
