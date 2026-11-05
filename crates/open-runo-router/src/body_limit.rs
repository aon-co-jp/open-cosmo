//! リクエスト本文のサイズ上限。
//!
//! `Request` の本文型(`Incoming`)は差し替えられないため、上限は2段で守る:
//! 1. `with_content_length_limit`: `Content-Length` が上限を超える要求を、
//!    本文を読む前に `413` で拒否するミドルウェア。
//! 2. `read_body_limited`: ハンドラ内で本文を読むときの上限つき読み取り。
//!    `Content-Length` の無い(chunked)送信や嘘の申告でも、上限を超えた
//!    時点で読むのを止める(全部溜めてから判定、をしない)。

#![allow(clippy::result_large_err)]

use crate::hyper_compat::{json_response, Handler, Request, Response};
use bytes::{Bytes, BytesMut};
use futures::StreamExt;
use http_body_util::BodyExt;
use hyper::StatusCode;
use std::sync::Arc;

fn too_large() -> Response {
    json_response(
        StatusCode::PAYLOAD_TOO_LARGE,
        &serde_json::json!({ "error": "request body too large" }),
    )
}

fn declared_length(req: &Request) -> Option<u64> {
    req.headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
}

/// 本文を `max` バイトまで読む。超えたら `413`、読み取り失敗は `400`。
pub async fn read_body_limited(req: Request, max: usize) -> Result<Bytes, Response> {
    if declared_length(&req).is_some_and(|len| len > max as u64) {
        return Err(too_large());
    }
    let mut stream = req.into_body().into_data_stream();
    let mut buf = BytesMut::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| {
            json_response(
                StatusCode::BAD_REQUEST,
                &serde_json::json!({ "error": "failed to read request body" }),
            )
        })?;
        if buf.len() + chunk.len() > max {
            return Err(too_large());
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf.freeze())
}

/// `Content-Length` が `max` を超える要求を、本文を読む前に `413` で拒否する。
/// 本文を読む側(ハンドラ)も [`read_body_limited`] か
/// `MultipartLimits` で上限を持つこと(chunked送信はここを素通りする)。
pub fn with_content_length_limit(inner: Handler, max: u64) -> Handler {
    Arc::new(move |req, params| {
        let inner = Arc::clone(&inner);
        Box::pin(async move {
            if declared_length(&req).is_some_and(|len| len > max) {
                return too_large();
            }
            inner(req, params).await
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hyper_compat::{empty_status, serve, Router};
    use hyper::Method;

    #[tokio::test]
    async fn limits_over_real_tcp_including_chunked() {
        let read: Handler = Arc::new(|req, _p| {
            Box::pin(async move {
                match read_body_limited(req, 1000).await {
                    Ok(b) => json_response(StatusCode::OK, &serde_json::json!({ "len": b.len() })),
                    Err(r) => r,
                }
            })
        });
        let guarded = with_content_length_limit(
            Arc::new(|_r, _p| Box::pin(async { empty_status(StatusCode::OK) })),
            100,
        );
        let router = Router::new().route(Method::POST, "/read", read).route(
            Method::POST,
            "/guarded",
            guarded,
        );
        let (addr, _h) = serve(router, "127.0.0.1:0".parse().unwrap()).await.unwrap();
        let c = reqwest::Client::new();

        let r = c
            .post(format!("http://{addr}/read"))
            .body(vec![0u8; 900])
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let r = c
            .post(format!("http://{addr}/read"))
            .body(vec![0u8; 1001])
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 413);

        // Content-Lengthなし(chunked)でも、読み取り中に上限で止まる。
        let frames: Vec<Result<hyper::body::Frame<Bytes>, std::io::Error>> = (0..5)
            .map(|_| Ok(hyper::body::Frame::data(Bytes::from(vec![0u8; 400]))))
            .collect();
        let body = reqwest::Body::wrap(http_body_util::StreamBody::new(futures::stream::iter(
            frames,
        )));
        let r = c
            .post(format!("http://{addr}/read"))
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 413);

        let r = c
            .post(format!("http://{addr}/guarded"))
            .body(vec![0u8; 101])
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 413);
        let r = c
            .post(format!("http://{addr}/guarded"))
            .body(vec![0u8; 100])
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
    }
}
