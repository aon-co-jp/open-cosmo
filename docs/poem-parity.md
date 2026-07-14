# Poem 機能パリティ調査(2026-07-11)

> hyper_compat(`crates/open-runo-router/src/hyper_compat.rs`)への移行後、
> 本家 Poem フレームワークが持つ機能に対して漏れがないかを調査した記録。
> 調査対象:
> - <https://docs.rs/poem/latest/poem/>(公式ドキュメント)
> - <https://github.com/poem-web/poem>(本体リポジトリ、周辺クレート一覧)
> - <https://zenn.dev/ouvill/articles/introduce_rust_poem_framework>(日本語入門記事)

## 1. Poem本体の主な機能

| カテゴリ | 内容 |
|---------|------|
| ルーティング | `Route`(パス)/`RouteDomain`(ドメイン)/`RouteMethod`(HTTPメソッド)/`RouteScheme`(スキーム) |
| エクストラクタ | `Query<T>`/`Path<T>`/`Json<T>`/`RemoteAddr`/`Method`/`Uri`、`FromRequest`でカスタム可 |
| レスポンス | `IntoResponse`トレイトで`String`/`StatusCode`/`Result`等を柔軟に変換 |
| エラーハンドリング | `catch_error()`、`NotFoundError`等の専用エラー型 |
| ミドルウェア | `with()`で適用、組み込み`Tracing`、`Middleware`トレイトでカスタム可 |
| WebSocket / SSE | プロトコルサポートあり(feature flag) |
| セキュリティ | Cookie・CSRF・TLS(native-tls/openssl/rustls)・ACME |
| データ処理 | Multipart・圧縮(gzip/br)・XML・YAML |
| セッション | Redisバックエンドセッション管理 |
| 監視 | OpenTelemetry・Prometheus・tokio-metrics |
| その他 | 静的ファイル埋め込み、テストユーティリティ(`TestClient`)、tower互換アダプタ、i18n(fluent) |
| 周辺クレート | `poem-openapi`(OpenAPI 3.0自動生成)・`poem-grpc`(gRPC)・`poem-lambda`(AWS Lambda)・`poem-mcpserver`(MCP Server)・`poem-worker` |

## 2. hyper_compat実装との対応表

| Poem機能 | hyper_compat対応 | 状態 |
|---------|------------------|------|
| ルーティング(パス+メソッド) | `hyper_compat::Router`(method+path、`:param`対応) | ✅ 実装済み |
| JSON抽出/レスポンス | `read_json_body`/`json_response` | ✅ 実装済み |
| クエリパラメータ | `hyper_compat::query_params` | ✅ 実装済み |
| ミドルウェア(CORS/RateLimit/Tracing) | `middleware_hyper::{with_cors, with_shared_rate_limit, with_tracing}` | ✅ 実装済み(関数コンビネータ方式) |
| SSE | `hyper_compat::sse_response` + `SseEvent` | ✅ 実装済み |
| 静的ファイル配信 | `hyper_compat::static_file_handler` | ✅ 実装済み(WASMフロントエンド配信用) |
| テストユーティリティ | `hyper_compat::serve` + `reqwest`(実HTTP経由) | ✅ 実装済み(TestClientの代替) |
| OpenAPI 3.0仕様生成 | `crates/open-runo-router/src/openapi.rs`(手書き静的JSON、`GET /api/openapi.json`) | ✅ 実装済み(2026-07-11追加。macro自動生成ではなく手書き) |
| ~~WebSocket(汎用)~~ | `hyper_compat::websocket_handler`(RFC 6455手書き実装) | ✅ 完了(2026-07-11)。`GET /api/ws-echo`(エコー)・`GET /api/ws-events`(state.eventsのWS版、認証必須)の2ルートで実証。GraphQL Subscriptions(poem版`graphql_route`)は引き続き別経路 |
| ~~gzip/br圧縮~~ | `middleware_hyper::with_compression`(gzip、`flate2`使用) | ✅ 完了(2026-07-11、gzipのみ・brは見送り。理由は3節参照) |
| ~~Multipart(ファイルアップロード)~~ | `hyper_compat::read_multipart_body` + `POST /api/schemas/upload` | ✅ 完了(2026-07-11)。RFC 7578手書きパーサー(`multer`等の外部crate不使用)。WASM管理UIに`<input type="file">`のアップロードUIを追加、実バイナリ+実ブラウザでファイル選択→アップロード→Schema Historyへの反映まで確認済み |
| ~~Cookie/セッション管理~~ | `session.rs`(`SessionStore`)+ `POST /api/session/login`・`POST /api/session/logout` | ✅ 完了(2026-07-12)。X-Api-Keyに追加する形の認証経路(置き換えではない)。既存キーを`/api/session/login`へ渡すとHttpOnly+SameSite=Strict Cookie+CSRFトークンを発行。**全ハンドラ(REST/SCIM/SSE/WebSocket含む)が対応済み**——`check_api_key`は完全に`authenticate_with_session`へ置換し、この関数・`actor_from_headers`ヘルパーごと削除(未参照になったため)。DB CRUD・Feature Flags・SCIM(Users/Groups)の3系統は実HTTPでCookie+CSRFのみ(X-Api-Key無し)によるPOST/PUT/GET/DELETEを個別に検証済み。既存のX-Api-Key専用テストは無変更・全てgreen(X-Api-Keyが存在する場合はそちらが優先されるため後方互換) |
| ~~CSRF保護~~ | `auth_hyper::authenticate_with_session`のdouble-submitトークン検証 | ✅ 完了(2026-07-12)。セッションCookie認証時、POST/PUT/PATCH/DELETEは`X-CSRF-Token`ヘッダがログイン時発行のトークンと一致しないと403(X-Api-Key認証時は対象外——ヘッダは自動送信されずCSRFの対象外のため)。実バイナリ+curlでCSRF無し403→CSRF有り200→logout後401を確認済み |
| ~~TLS(rustls termination)~~ | `hyper_compat::tls::{load_tls_config, serve_tls}` | ✅ 完了(2026-07-12)。`tls` Cargo feature(既定オフ、リバースプロキシ前提のデプロイに不要な依存を持ち込まない)。自己署名証明書(`rcgen`はテスト専用)+実TLSハンドシェイク+平文HTTPクライアントが拒否されることを実バイナリ相当のテストで確認済み |
| ~~ACME(自動証明書発行)~~ | `crates/open-runo-router/src/acme.rs`(`AcmeClient`、`acme` feature) | ✅ 完了(2026-07-12)。RFC 8555のHTTP-01チャレンジ経路(directory→nonce→account→order→authorization→challenge→finalize→download)を手書き実装。JWS署名は`ring`のES256(ECDSA P-256/SHA-256、fixed r‖s形式)を使用(生の楕円曲線演算は自前実装せず、既にこのcrateがWebSocket/APIキーハッシュでsha1/sha2を使う方針と同じ境界線)。`ChallengeStore`+`GET /.well-known/acme-challenge/:token`は`acme` feature非依存で常時コンパイル。**検証範囲の明記**: HTTP-01検証はCA側からこのサーバーへ公開インターネット経由で到達できる必要があり、この開発環境(サンドボックス、公開ドメイン無し)では実運用のLet's Encryptに対する最終確認はできない。代わりに、`hyper_compat::serve`で構築した実チャレンジ応答サーバー(本番と同じ`ChallengeStore`/`challenge_response_handler`)と、実際にそのサーバーへ実HTTPで公開鍵認証をフェッチしに行くモックCAサーバーの、2つの独立プロセス間の実HTTPラウンドトリップで全フローを検証(`acme::client::tests::full_http01_flow_against_mock_ca_with_real_challenge_loopback`)。ES256署名が正しいfixed-length形式(64バイト、ASN.1 DERではない)であることも単体テストで確認済み |
| ~~gRPC(poem-grpc相当)~~ | `crates/open-runo-router/src/grpc.rs`(`serve_grpc`、常時コンパイル・新規依存無し) | ✅ 完了(2026-07-12、同日中にストリーミング追加)。`grpc.health.v1.Health/Check`(unary)に加え`/Watch`(サーバーストリーミング)を実装し、「ストリーミング未対応」のギャップを解消。実HTTP/2(h2c、prior knowledge)+手書きProtocol Buffersコーデック(この2メッセージ分のみ)で実装。HTTP/2自体はhyperの既存`full`feature(`h2`crate、新規依存無し)を利用——WebSocket/multipartと同じ「プロトコル・データ形状は手書き、監査済みライブラリが要る部分(暗号)以外は自前実装」の方針。専用ポート(`OPEN_RUNO_GRPC_BIND_ADDR`、未設定なら起動しない)。CargoのバグではなくCIサンドボックスの制約(edition2024を要求する無関係な推移的依存の解決失敗)により本セッションのcargoではワークスペース全体のビルド確認ができなかったため、`grpc.rs`を依存バージョンを固定した独立クレートに切り出して検証: 実HTTP/2サーバーを起動し、独立した`hyper-util`クライアントで`Check`(unary)・`Watch`(streaming、trailers経由のgrpc-status・protobufバイト列を含む)双方の実ラウンドトリップを確認、9件全テスト合格。grpcurl等の外部ツールでの追加検証はこの環境に無かったため未実施 |
| ~~ACME TLS-ALPN-01チャレンジ(RFC 8737)~~ | `crates/open-runo-router/src/acme.rs`(`tls_alpn01`サブモジュール、`acme` feature) | ✅ 完了(2026-07-12)。HTTP-01と異なりポート80への到達性が不要——ACME CAは通常のTLSポートへ直接接続し`acme-tls/1` ALPNプロトコルをネゴシエートする。サーバーは実データを一切送受信せず、鍵認証のSHA-256ダイジェストを含むcritical拡張(id-pe-acmeIdentifier、OID `1.3.6.1.5.5.7.1.31`)を持つ使い捨て自己署名証明書を提示するだけ。既存の`hyper_compat::tls`と同じrustlsスタックを再利用し、`rustls::server::ResolvesServerCert`をClientHelloのALPN要求で分岐させる`TlsAlpnResolver`を新設——`acme-tls/1`が要求された接続だけ検証用証明書を返し、それ以外(通常のHTTPS含む)は`fallback`証明書のまま。**検証**: 証明書生成の単体テスト(ダイジェストがOCTET STRINGとしてDERに含まれること、異なるダイジェストで異なる証明書になること)に加え、`resolver_serves_validation_cert_only_for_acme_tls_alpn1_connections`で**実TCP+実rustls TLSハンドシェイクを2回**行い、`acme-tls/1`で接続した場合のみ検証用証明書が返り、`http/1.1`で接続した場合はfallback証明書が返ることを`peer_certificates()`の実バイト比較で確認(実装中、TLSセッションチケット再開により2回目の接続が1回目の証明書を再利用してしまう実バグを発見——各接続ごとに独立した`ServerConfig`を使う形に修正して解消)。CAオーケストレーション(`obtain_certificate`)はHTTP-01の`obtain_certificate_http01`と同型で、公開ドメイン到達性が無いこの環境では実CAに対する最終確認はできない(HTTP-01と同じ制約) |
| ~~ACME DNS-01チャレンジ(RFC 8555 §8.4)~~ | `crates/open-runo-router/src/acme.rs`(`dns01`サブモジュール、`acme` feature) | ✅ 完了(2026-07-14)。3チャレンジ型のうち唯一、ドメインに一切サーバーを立てず(ポート80もTLSポートも不要)DNSレコードの存在だけで検証でき、ワイルドカード証明書にも対応できる方式。トレードオフとして実DNSゾーンへの書き込み権限が必要——このサンドボックスには実DNSゾーン・プロバイダ資格情報が無いため、実プロバイダとの相互運用は最終確認できない(HTTP-01/TLS-ALPN-01と同じ制約が一段階外側に移動しただけ)。**DNSプロバイダは小さな`DnsProvider` trait**(`create_txt_record`/`delete_txt_record`)として抽象化し、実装例としてCloudflare REST API(bearerトークンのみ・署名不要で最も単純な実プロバイダAPIの一つ)向け`CloudflareDnsProvider`を用意——プレースホルダではなく実際に使えるプロバイダ実装。TXT値の計算(base64url(SHA-256(鍵認証))、レコード名`_acme-challenge.<domain>`)はRFC 8555 §8.4準拠。**検証**: `CloudflareDnsProvider`単体のcreate/delete往復を`hyper_compat::serve`によるモックCloudflare APIサーバーに対する実HTTPで確認(未作成のレコードに対するdeleteが実際に何もHTTPリクエストを送らないno-opであることも含む)。加えてHTTP-01と同じモックCA手法+このモックCloudflareサーバーを組み合わせた`full_dns01_flow_against_mock_ca_and_mock_dns_provider`で、TXTレコード公開→ACMEチャレンジ応答→検証→クリーンアップという**順序自体**をCA側のモック検証ロジック(TXTレコードが実際に作成された後でなければchallengeをvalidにしない)で実証 |
| ~~MCP Server(poem-mcpserver相当)~~ | `crates/open-runo-router/src/mcp.rs`(`POST /mcp`、新規依存無し) | ✅ 完了(2026-07-12)。Streamable HTTP transportの単純系(1リクエスト→1レスポンス、SSE無し)でJSON-RPC 2.0を実装。`initialize`/`tools/list`/`tools/call`/`resources/list`/`resources/read`に対応。実ツール2種(`health_check`・`self_issue_api_key`)+実リソース2種(`openapi://spec`・`health://status`)——いずれも既存の`GET /health`・`POST /api/keys/self-issue`・`GET /api/openapi.json`と同じ本番ロジック・データを共有し、MCP専用のスタブや別データソースではない(単体テストでOpenAPIリソースがREST版と完全一致することを直接比較して確認)。実バイナリ+curlでinitialize→tools/list→tools/call→resources/list→resources/readの一連を確認済み。**Prompts対応も完了(2026-07-12)**——`prompts/list`(1件: `summarize_api`)・`prompts/get`。`summarize_api`はダミーではなく、Resourcesと同じ`openapi::spec()`(実際に現在稼働中のOpenAPIドキュメント)をレンダリングしたユーザーメッセージを返す`GetPromptResult`形状のJSONを生成——プロンプト定義時点のスナップショットではなく常に最新のAPI仕様を埋め込む。単体テストで、返るメッセージ本文が実際の`openapi::spec()`の出力を包含していることを直接比較で確認(スタブでないことの証明)。実バイナリ+curlでprompts/list→prompts/get(summarize_api)→prompts/get(未知の名前、-32602エラー)を確認済み |

## 3. 優先度付きギャップ一覧

| 項目 | 優先度 | 理由 |
|------|--------|------|
| ~~gzip/br圧縮ミドルウェア~~ | ★★☆ | ✅ 完了(2026-07-11)。本番運用のパフォーマンス向上に直結。`GET /api/openapi.json`で実測10265→2115バイト(約79%削減)を確認 |
| ~~汎用WebSocket対応~~ | ★★☆ | ✅ 完了(2026-07-11)。RFC 6455ハンドシェイク・フレーミングを`sha1`のみでhyper_compatに手書き実装、実バイナリ+実WSクライアント(Node.js `WebSocket`)でエコーの往復を確認 |
| ~~Multipart/ファイルアップロード~~ | ★☆☆ | ✅ 完了(2026-07-11)。`POST /api/schemas/upload`でSDLファイルの直接アップロードに対応 |
| ~~Cookie/セッション + CSRF~~ | ★☆☆ | ✅ 完了(2026-07-12)。X-Api-Key認証への追加経路として実装(置き換えではない) |
| ~~ACME(自動証明書発行)~~ | ★★☆ | ✅ 完了(2026-07-12)。HTTP-01のみ(DNS-01/TLS-ALPN-01は未対応) |
| ~~gRPC対応~~ | ★☆☆ | ✅ 完了(2026-07-12)。`grpc.health.v1.Health/Check`(unary)・`/Watch`(streaming)・`grpc.reflection.v1.ServerReflection`(`list_services`のみ)。他サービス・他リクエスト種別は未対応 |
| ~~Multipart以外のファイル添付~~ | ★☆☆ | ✅ 完了(2026-07-12)。`POST /api/schemas/upload-raw`で生バイト列(`Content-Type: application/octet-stream`等)+クエリパラメータ(`service_name`必須、`stage`/`namespace`任意)による登録に対応。JSON・Multipartと同じ`register_schema_and_respond`に集約しているため3経路とも同一の登録結果に収束する |
| ~~MCP Server対応~~ | ★☆☆ | ✅ 完了(2026-07-12) |

## 4. 結論

hyper_compatはPoemの**コア機能(ルーティング・エクストラクタ・レスポンス・
ミドルウェア・SSE・静的配信・テスト・Multipart・Cookie/セッション+CSRF・
TLS・ACME・gRPC・MCP Server)を実用上必要十分にカバー**している。gzip
圧縮・汎用WebSocket・Multipartファイルアップロード・Cookie/セッション
管理・CSRF保護・TLS終端・ACME(HTTP-01)・gRPC(grpc.health.v1.Health)・
MCP Server(Tools)はいずれも2026-07-11〜12に実装完了。`docs/poem-parity.md`
4a節時点で列挙していたギャップは全て解消し、MCP ServerもTools/
Resources/Prompts全て完了(2026-07-12)。ACMEもHTTP-01に加えTLS-ALPN-01
チャレンジが完了(2026-07-12)。gRPCもストリーミング(`Health/Watch`)・
リフレクション(`ServerReflection`、`list_services`のみ)・未知サービス名への
`NOT_FOUND`応答を同日中に追加(2026-07-12)。Multipart以外のファイル添付
(`POST /api/schemas/upload-raw`、生バイト列 + クエリパラメータ)も完了
(2026-07-12)。ACMEもDNS-01チャレンジが完了(2026-07-14、`DnsProvider`
trait+Cloudflare REST API実装)し、3チャレンジ型(HTTP-01/TLS-ALPN-01/
DNS-01)すべてに対応済み。残るのは各機能の対応範囲拡大(gRPCの他
サービス・`file_containing_symbol`等の他リクエスト種別)のみ。
