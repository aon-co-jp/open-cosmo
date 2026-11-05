//! gRPC クライアント(CLI・呼び出し側用。`client` feature)。
//!
//! HTTP/2 で `open_runo.admin.v1.Admin` を呼ぶ。`http://`は平文(h2c、開発・閉域用)、`https://`は
//! **TLS 1.3 のみ**(ALPN `h2`)。CA は PEM で渡す(未指定なら`webpki-roots`)。管理系の相互認証(mTLS)は
//! クライアント証明書+鍵のPEMを渡す。認証は`x-api-key`メタデータ(またはCookie)。

use crate::{code, encode_frame, method_path, split_frame};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::client::conn::http2::SendRequest;
use hyper_util::rt::{TokioExecutor, TokioIo};
use prost::Message;
use std::sync::Arc;
use tokio::sync::Mutex;

/// gRPCのステータス(コードとメッセージ)。
#[derive(Debug, Clone)]
pub struct Status {
    pub code: i32,
    pub message: String,
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

impl Status {
    fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// 接続設定。
#[derive(Debug, Clone, Default)]
pub struct ClientConfig {
    /// `http://host:port`(平文h2c)または`https://host:port`(TLS1.3)。
    pub endpoint: String,
    /// `x-api-key`メタデータ。
    pub api_key: Option<String>,
    /// サーバー証明書を検証するCA(PEM)。未指定なら`webpki-roots`。
    pub ca_pem: Option<Vec<u8>>,
    /// mTLS用のクライアント証明書チェーン(PEM)と秘密鍵(PEM)。
    pub client_cert_pem: Option<Vec<u8>>,
    pub client_key_pem: Option<Vec<u8>>,
    /// 追加のメタデータ(Cookie/CSRFなど)。
    pub extra_metadata: Vec<(String, String)>,
}

/// 応答メタデータ(ヘッダ)。
pub type Metadata = hyper::HeaderMap;

/// gRPCクライアント。接続は最初の呼び出しで張り、HTTP/2で多重化して使い回す。
#[derive(Clone)]
pub struct GrpcClient {
    cfg: Arc<ClientConfig>,
    conn: Arc<Mutex<Option<SendRequest<Full<Bytes>>>>>,
}

impl std::fmt::Debug for GrpcClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrpcClient")
            .field("endpoint", &self.cfg.endpoint)
            .finish()
    }
}

impl GrpcClient {
    pub fn new(cfg: ClientConfig) -> Self {
        Self {
            cfg: Arc::new(cfg),
            conn: Arc::new(Mutex::new(None)),
        }
    }

    /// 呼び出しに使う鍵を差し替えたクライアント(接続は共有しない)。
    pub fn with_api_key(&self, key: Option<String>) -> Self {
        let mut cfg = (*self.cfg).clone();
        cfg.api_key = key;
        Self::new(cfg)
    }

    fn authority(&self) -> Result<(bool, String, String), Status> {
        let ep = self.cfg.endpoint.trim_end_matches('/');
        let (tls, rest) = if let Some(r) = ep.strip_prefix("https://") {
            (true, r)
        } else if let Some(r) = ep.strip_prefix("http://") {
            (false, r)
        } else {
            return Err(Status::new(
                code::INVALID_ARGUMENT,
                "endpoint must start with http:// or https://",
            ));
        };
        let host = rest.rsplit_once(':').map(|(h, _)| h).unwrap_or(rest);
        Ok((
            tls,
            rest.to_string(),
            host.trim_matches(|c| c == '[' || c == ']').to_string(),
        ))
    }

    fn tls_config(&self) -> Result<rustls::ClientConfig, Status> {
        let err = |m: String| Status::new(code::INVALID_ARGUMENT, m);
        let mut roots = rustls::RootCertStore::empty();
        match &self.cfg.ca_pem {
            Some(pem) => {
                for c in rustls_pemfile::certs(&mut &pem[..]) {
                    roots
                        .add(c.map_err(|e| err(format!("bad CA pem: {e}")))?)
                        .map_err(|e| err(format!("bad CA: {e}")))?;
                }
            },
            None => roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned()),
        }
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let builder = rustls::ClientConfig::builder_with_provider(provider)
            // TLS 1.3 必須(1.2以下は使わない)。
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|e| err(e.to_string()))?
            .with_root_certificates(roots);
        let mut cfg = match (&self.cfg.client_cert_pem, &self.cfg.client_key_pem) {
            (Some(cert), Some(key)) => {
                let chain: Vec<_> = rustls_pemfile::certs(&mut &cert[..])
                    .collect::<Result<_, _>>()
                    .map_err(|e| err(format!("bad client cert: {e}")))?;
                let key = rustls_pemfile::private_key(&mut &key[..])
                    .map_err(|e| err(format!("bad client key: {e}")))?
                    .ok_or_else(|| err("no private key in client key pem".into()))?;
                builder
                    .with_client_auth_cert(chain, key)
                    .map_err(|e| err(e.to_string()))?
            },
            (None, None) => builder.with_no_client_auth(),
            _ => return Err(err("client cert and key must be given together".into())),
        };
        cfg.alpn_protocols = vec![b"h2".to_vec()];
        Ok(cfg)
    }

    async fn connect(&self) -> Result<SendRequest<Full<Bytes>>, Status> {
        let (tls, addr, host) = self.authority()?;
        let unavailable = |e: String| Status::new(14, format!("cannot connect to {addr}: {e}"));
        let tcp = tokio::net::TcpStream::connect(&addr)
            .await
            .map_err(|e| unavailable(e.to_string()))?;
        let _ = tcp.set_nodelay(true);
        if tls {
            let connector = tokio_rustls::TlsConnector::from(Arc::new(self.tls_config()?));
            let name = rustls::pki_types::ServerName::try_from(host.clone()).map_err(|e| {
                Status::new(
                    code::INVALID_ARGUMENT,
                    format!("bad server name {host}: {e}"),
                )
            })?;
            let stream = connector
                .connect(name, tcp)
                .await
                .map_err(|e| unavailable(format!("TLS: {e}")))?;
            let (send, conn) =
                hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(stream))
                    .await
                    .map_err(|e| unavailable(e.to_string()))?;
            tokio::spawn(async move {
                let _ = conn.await;
            });
            Ok(send)
        } else {
            let (send, conn) =
                hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(tcp))
                    .await
                    .map_err(|e| unavailable(e.to_string()))?;
            tokio::spawn(async move {
                let _ = conn.await;
            });
            Ok(send)
        }
    }

    async fn sender(&self) -> Result<SendRequest<Full<Bytes>>, Status> {
        let mut guard = self.conn.lock().await;
        if let Some(s) = guard.as_ref() {
            if !s.is_closed() {
                return Ok(s.clone());
            }
        }
        let s = self.connect().await?;
        *guard = Some(s.clone());
        Ok(s)
    }

    /// 生のバイト列で呼ぶ。`(応答本体のフレーム列, 応答メタデータ)`。`grpc-status`が0以外ならエラー。
    pub async fn call_raw(
        &self,
        method: &str,
        request: &[u8],
    ) -> Result<(Vec<Vec<u8>>, Metadata), Status> {
        self.call_raw_path(&method_path(method), request).await
    }

    /// パスを直接指定して呼ぶ(リフレクションなど管理サービス以外にも使う)。
    pub async fn call_raw_path(
        &self,
        path: &str,
        request: &[u8],
    ) -> Result<(Vec<Vec<u8>>, Metadata), Status> {
        let (_tls, addr, _) = self.authority()?;
        let scheme = if self.cfg.endpoint.starts_with("https://") {
            "https"
        } else {
            "http"
        };
        let mut builder = hyper::Request::builder()
            .method("POST")
            .uri(format!("{scheme}://{addr}{path}"))
            .header("content-type", "application/grpc+proto")
            .header("te", "trailers");
        if let Some(k) = &self.cfg.api_key {
            builder = builder.header("x-api-key", k.as_str());
        }
        for (k, v) in &self.cfg.extra_metadata {
            builder = builder.header(k.as_str(), v.as_str());
        }
        let req = builder
            .body(Full::new(Bytes::from(encode_frame(request))))
            .map_err(|e| Status::new(code::INVALID_ARGUMENT, e.to_string()))?;

        // 接続が切れていたら1回だけ張り直す。
        let resp = match self.sender().await?.send_request(req.clone()).await {
            Ok(r) => r,
            Err(_) => {
                *self.conn.lock().await = None;
                self.sender()
                    .await?
                    .send_request(req)
                    .await
                    .map_err(|e| Status::new(14, format!("request failed: {e}")))?
            },
        };
        let (parts, body) = resp.into_parts();
        let collected = body
            .collect()
            .await
            .map_err(|e| Status::new(code::INTERNAL, format!("body: {e}")))?;
        let trailers = collected.trailers().cloned();
        let data = collected.to_bytes();
        // トレーラー(正常系)かヘッダ(Trailers-Only)に grpc-status が載る。
        let status_src = trailers.as_ref().unwrap_or(&parts.headers);
        let status = status_src
            .get("grpc-status")
            .or_else(|| parts.headers.get("grpc-status"))
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<i32>().ok())
            .unwrap_or(code::UNKNOWN);
        if status != code::OK {
            let msg = status_src
                .get("grpc-message")
                .or_else(|| parts.headers.get("grpc-message"))
                .and_then(|v| v.to_str().ok())
                .map(crate::percent_decode)
                .unwrap_or_default();
            return Err(Status::new(status, msg));
        }
        let mut frames = Vec::new();
        let mut rest: &[u8] = &data;
        while let Some((flag, body, tail)) = split_frame(rest) {
            if flag == 0 {
                frames.push(body.to_vec());
            }
            rest = tail;
        }
        Ok((frames, parts.headers))
    }

    /// 単項呼び出し。
    pub async fn unary<Req: Message, Resp: Message + Default>(
        &self,
        method: &str,
        request: &Req,
    ) -> Result<Resp, Status> {
        self.unary_with_metadata(method, request)
            .await
            .map(|(r, _)| r)
    }

    /// 単項呼び出し(応答メタデータ付き。`SessionLogin`の`set-cookie`用)。
    pub async fn unary_with_metadata<Req: Message, Resp: Message + Default>(
        &self,
        method: &str,
        request: &Req,
    ) -> Result<(Resp, Metadata), Status> {
        let (frames, md) = self.call_raw(method, &request.encode_to_vec()).await?;
        let first = frames.into_iter().next().unwrap_or_default();
        let resp = Resp::decode(&first[..])
            .map_err(|e| Status::new(code::INTERNAL, format!("bad response: {e}")))?;
        Ok((resp, md))
    }

    /// サーバーストリーミング。ストリームは`max`件受け取るか`timeout`で打ち切る
    /// (無限ストリームの`WatchEvents`向けに、受信中のデータだけを読む)。
    pub async fn stream_some<Req: Message, Resp: Message + Default>(
        &self,
        method: &str,
        request: &Req,
        max: usize,
        timeout: std::time::Duration,
    ) -> Result<Vec<Resp>, Status> {
        use http_body_util::BodyExt as _;
        let (_tls, addr, _) = self.authority()?;
        let scheme = if self.cfg.endpoint.starts_with("https://") {
            "https"
        } else {
            "http"
        };
        let mut builder = hyper::Request::builder()
            .method("POST")
            .uri(format!("{scheme}://{addr}{}", method_path(method)))
            .header("content-type", "application/grpc+proto")
            .header("te", "trailers");
        if let Some(k) = &self.cfg.api_key {
            builder = builder.header("x-api-key", k.as_str());
        }
        let req = builder
            .body(Full::new(Bytes::from(encode_frame(
                &request.encode_to_vec(),
            ))))
            .map_err(|e| Status::new(code::INVALID_ARGUMENT, e.to_string()))?;
        let resp = self
            .sender()
            .await?
            .send_request(req)
            .await
            .map_err(|e| Status::new(14, format!("request failed: {e}")))?;
        let (parts, mut body) = resp.into_parts();
        if let Some(s) = parts
            .headers
            .get("grpc-status")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<i32>().ok())
        {
            if s != code::OK {
                return Err(Status::new(s, String::new()));
            }
        }
        let mut buf: Vec<u8> = Vec::new();
        let mut out = Vec::new();
        let deadline = tokio::time::Instant::now() + timeout;
        while out.len() < max {
            let next = tokio::time::timeout_at(deadline, body.frame()).await;
            let Ok(Some(Ok(frame))) = next else { break };
            if let Some(data) = frame.data_ref() {
                buf.extend_from_slice(data);
                while let Some((flag, msg, tail)) = split_frame(&buf.clone()) {
                    if flag == 0 {
                        out.push(Resp::decode(msg).map_err(|e| {
                            Status::new(code::INTERNAL, format!("bad message: {e}"))
                        })?);
                    }
                    let consumed = buf.len() - tail.len();
                    buf.drain(..consumed);
                }
            } else if let Some(t) = frame.trailers_ref() {
                let s = t
                    .get("grpc-status")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<i32>().ok())
                    .unwrap_or(0);
                if s != code::OK {
                    let m = t
                        .get("grpc-message")
                        .and_then(|v| v.to_str().ok())
                        .map(crate::percent_decode)
                        .unwrap_or_default();
                    return Err(Status::new(s, m));
                }
                break;
            }
        }
        Ok(out)
    }
}
