# gRPC-web-Connect

**Connect と gRPC-web の「良いとこ取り」ハイブリッド**を、サーバー(Rust/hyper)とブラウザ(Rust→WASM)の
両方で提供するライブラリです。1つのパス・1つのサービス定義で、次の**5つの通信方式**をContent-Typeで判別して扱います。

| 方式 | Content-Type | 向き |
|---|---|---|
| gRPC | `application/grpc[+proto]` | サーバー間・CLI(HTTP/2のトレーラー) |
| gRPC-web | `application/grpc-web[+proto]` | ブラウザ(トレーラーを本文の末尾フレームに置く) |
| gRPC-web-text | `application/grpc-web-text[+proto]` | 古いブラウザ/XHR(各フレームをbase64) |
| Connect(単項) | `application/proto` / `application/json` | ブラウザ・**curl**(素の本文、エラーはHTTPステータス) |
| Connect(ストリーム) | `application/connect+proto` / `application/connect+json` | ブラウザ(エンベロープ+終端フレーム) |

## なぜハイブリッドか

| | Connect | gRPC-web | **このライブラリ** |
|---|---|---|---|
| curl・開発者ツールで読める | ◎(素のHTTP+JSON) | ✕(バイナリ+末尾トレーラー) | ◎ 単項はConnect |
| エラーがHTTPステータスで見える | ◎ | ✕(常に200) | ◎ |
| gRPCのコード体系・既存ツール | △ | ◎ | ◎ Connectのエラーにも`grpc-status`/`grpc-message`ヘッダを付ける |
| プロキシ・古い環境での互換 | △ | ◎ | ◎ Connectを話さないサーバーなら**自動でgRPC-webへ切り替え** |
| ブラウザでサーバーストリーミング | ○ | ○ | ◎ `ReadableStream`+`AbortController`、複数を同時に |
| 同じ処理を方式ごとに書き直す | - | - | **不要**(`Service::call`は方式を知らない) |

## 構成

- `crates/core`(`open-runo-connect-core`): 方式の判別、フレーム、gRPC-webトレーラー、Connect終端フレーム、
  base64テキスト、ステータスコードの対応(gRPC⇔HTTP⇔Connect)。HTTPスタックに依存せず**wasm32でも動く**。
- `crates/server`(`open-runo-connect-server`): `Service`を実装して`handle`に渡すだけ。proto/JSONの両コーデック
  (JSONは記述子から`prost-reflect`で変換、`json` feature)、単項・サーバーストリーミング。
- `crates/client`(`open-runo-connect-client`): ブラウザ用。単項=Connect、ストリーム=`ReadableStream`、
  自動フォールバック、並列実行、呼び出しの記録(インスペクタ用)、`debug`で開発者ツールのConsoleにも出力。

## 使い方(サーバー)

```rust
use async_trait::async_trait;
use open_runo_connect_server::{handle, MethodInfo, Output, Service};
use open_runo_connect_server::proto_core::{code, Status};

struct Demo;
#[async_trait]
impl Service for Demo {
    fn service_name(&self) -> &str { "demo.v1.Demo" }
    fn method(&self, name: &str) -> Option<MethodInfo> {
        (name == "Echo").then(MethodInfo::unary) // JSONも使うなら MethodInfo::with_descriptors(..)
    }
    async fn call(&self, _method: &str, headers: &hyper::HeaderMap, msg: &[u8]) -> Result<Output, Status> {
        if headers.get("x-api-key").is_none() {
            return Err(Status::new(code::UNAUTHENTICATED, "key required"));
        }
        Ok(Output::unary(msg.to_vec()))
    }
}
// hyperのサービス関数の中で: handle(&Demo, req).await
```

`async-trait`は利用側の依存に加えてください(`use async_trait::async_trait;`)。

## 使い方(ブラウザ/WASM)

```rust
let client = open_runo_connect_client::Client::new("demo.v1.Demo"); // 同一オリジン
client.set_header("x-api-key", &key);
let reply = client.unary("Echo", &request_bytes).await?;       // Connect(失敗時はHTTPステータスが見える)
let h = client.server_stream("Watch", &req, |msg| { /* … */ }, |end| { /* … */ })?; // ストリーム
h.stop();                                                        // 中断
client.set_debug(true);                                          // 開発者ツールのConsoleにも出す
```

## curlで叩く

```bash
curl -s -X POST http://localhost:8080/demo.v1.Demo/Echo \
  -H 'content-type: application/json' -H 'x-api-key: KEY' -d '{"text":"hi"}'
```

## 状況

- 出自: [`RPoem`](https://github.com/aon-co-jp/RPoem)の管理サービス(`open_runo.admin.v1.Admin`)で実機確認した
  方式(管理画面10ページ、connect-rustのWASMクライアント、実HTTP/2のCLI)を、汎用ライブラリとして切り出した。
- テスト: `cargo test`(core 4件・server 6件。5方式の単項/ストリーム/エラー/圧縮拒否/取り違えを網羅)。
  clientは`cargo check --target wasm32-unknown-unknown`(ブラウザでの動作はRPoemの管理画面で確認済み)。
- 未了・制限: 圧縮(gzip等)は未対応(identityのみ、要求されたら`unimplemented`)。クライアント/双方向
  ストリーミングはブラウザのfetchの制約で不可。TLS・HTTP/2の待ち受けは利用側(hyper)の責務。
  RPoem側は当面、同等のコードを自前で持つ(このライブラリへの置き換えは`PORTING.md`の次の一手)。

ライセンス: Apache-2.0 OR MIT
