# PORTING.md — open-runo お引越しファイル

> このファイル 1 枚で、**どのプロジェクトでも open-runo を導入・移設**できます。
> open-e-gov / OpenRedmine / OpenWordPress など新プロジェクトのリポジトリに
> このファイルをコピーして、上から順に進めてください。
>
> 対象バージョン: workspace 0.1.0（18 クレート / 262 テスト、2026-07-11
> 実測。最新のクレート数・テスト数は `CLAUDE.md` の「現状」節参照）
> 最終更新: 2026-07-11

---

## 0. open-runo と poem-cosmo-tauri の違い(2026-07-11、ユーザーによる最終定義)

このリポジトリ `open-runo` は、姉妹リポジトリ `poem-cosmo-tauri`
(https://github.com/aon-co-jp/poem-cosmo-tauri) と共通コア(下記「cosmo
部分」)を持つが、**全く違うリポジトリのプロジェクト**であり統合対象では
ない。poem-cosmo-tauri はこの共通コアに加えて、(1) Rust の Poem
フレームワークの全機能を完全互換で一から自作・再現したバックエンド、
(2) Tauri の全機能を完全互換で一から自作・再現したフロントエンド、を
持ち、その2つとcosmoコアの3者をスムーズに連携させることで効率的な
WEB開発を実現するフレームワーク/ミドルウェアという、より広いスコープを
持つ。**このリポジトリ(open-runo)にその上乗せスコープはなく、以下の
cosmo部分が中心**。詳細は `CLAUDE.md` の該当節、または open-raid-z の
`CLAUDE.md`(正本)を参照。

## 1. open-runo とは（cosmo部分、30 秒版）

Rust + tokio/hyper 製（`poem`パッケージには依存しない）の
**GraphQL Federation プラットフォーム / Web フレームワーク**。
WunderGraph Cosmo の有料版（Launch / Scale / Enterprise）機能を OSS で提供し、
さらに独自の自己学習 AI（外部 LLM 契約不要）を搭載します。

| 分類 | 提供機能 |
|------|----------|
| GraphQL | Federation 合成・破壊的変更検出・`POST /graphql`・Subscriptions (WS)・GraphiQL |
| Cosmo 有料版互換 | Persisted Queries (Trusted Documents)・厳密 RBAC・OIDC SSO・SCIM 2.0 (Users/Groups)・監査ログ・細粒度レートリミット・レスポンスキャッシュ・マルチグラフ namespace |
| 独自 AI（LLM 不要） | HTML ページキャッシュの自動判定/適応 TTL/先回り再生成、API キーの異常検知・自動隔離 |
| 自己運用 | 鍵の自動発行/失効（SCIM 連動）、AI 学習の自動永続化、両 DB 整合性の自動検証・自動修復、二か所自動バックアップ |
| お引越し | エンジン間変換（MySQL→PostgreSQL 等）、SQL/CSV エクスポート（Snowflake）、分散 DB 統合（Federated）、ワンコール復活 |

## 2. 持っていくもの（ファイル一覧）

```
open-runo/
├── Cargo.toml / Cargo.lock      ← workspace 定義（バージョン固定）
├── crates/                      ← 18 クレート（本体）
├── apps/desktop-wasm/            ← Rust→WebAssembly 管理アプリ（任意、open-runo-routerが自前配信）
├── docs/                        ← 設計・API 仕様・migration.md ほか
├── .github/workflows/ci.yml     ← fmt / clippy -D warnings / test
├── Dockerfile / docker-compose.yml / Makefile
└── PORTING.md                   ← 本ファイル
```

丸ごと移設する場合はフォルダごとコピーして `cargo test --workspace`
（262 テストが通れば移設成功）。以下はライブラリとして使う場合です。

## 3. 依存の書き方（新プロジェクトの Cargo.toml）

```toml
[dependencies]
# 同一マシンにある場合（path 依存）
open-runo-core     = { path = "../open-runo/crates/open-runo-core" }
open-runo-router   = { path = "../open-runo/crates/open-runo-router" }
open-runo-gateway  = { path = "../open-runo/crates/open-runo-gateway" }
open-runo-db       = { path = "../open-runo/crates/open-runo-db" }
open-runo-cache    = { path = "../open-runo/crates/open-runo-cache" }
open-runo-security = { path = "../open-runo/crates/open-runo-security" }

# GitHub 公開後は git 依存でも可
# open-runo-router = { git = "https://github.com/aon-co-jp/poem-cosmo-tauri" }

tokio = { version = "1.40", features = ["full"] }
hyper = { version = "1.10", features = ["full"] }
# poem は不要(open-runo-routerは4.1のようにtokio/hyperで直接動く)。
# GraphQL Subscriptions(WebSocket)が必要な場合のみ追加:
# poem = { version = "3.1", features = ["sse"] }
```

必要なものだけ選べます（各クレートは独立してテスト可能）。
DB エンジンは feature で選択: `open-runo-db = { ..., features = ["dual"] }`
（`postgres` / `mysql` / `sqlite` / `aruaru` / `cockroach` / `yugabyte` /
`mongodb` / `redis` / `clickhouse` / 複合: `dual` `single-pg` `single-my`
`dev` `full` `cluster`）。

## 4. 組み込みレシピ

### 4.1 フルスタック（REST + GraphQL + 自己運用ぜんぶ）

`open-runo-gateway`の`main.rs`をそのまま流用するのが最速です(poem非依存、
tokio/hyperベース):

```rust
use open_runo_core::Config;
use open_runo_router::{build_hyper_app, hyper_compat, state::AppState};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::from_env()?;
    open_runo_observability::init_tracing(&config.log_level);

    let state = Arc::new(AppState::new()); // 本番: AppState::with_db(...)
    let (graphiql, graphql_post) =
        open_runo_gateway::graphql_hyper::graphql_handlers(Arc::clone(&state));

    let app = build_hyper_app(
        state,
        config.rate_limit_max_requests,
        config.rate_limit_window_secs as i64,
    )
    .route(hyper::Method::GET, "/graphql", graphiql)
    .route(hyper::Method::POST, "/graphql", graphql_post);

    let addr = config.bind_addr.parse()?;
    let (_, handle) = hyper_compat::serve(app, addr).await?;
    handle.await?;
    Ok(())
}
```

GraphQL Subscriptions（WebSocket）が必要な場合のみ、代わりに
`open_runo_gateway::graphql_route(state)`（poem版、`/graphql/ws`対応）を
使ってください — その場合`poem`への依存が復活します。

これだけで、認証（KeyGuardian 自動運用）・RBAC・OIDC・SCIM・監査ログ・
AI モデル自動永続化・整合性自動修復・定期バックアップが**全部背景で動きます**。

### 4.2 自分のページに AI HTML キャッシュだけ載せる（OpenWordPress 型）

```rust
use open_runo_router::middleware::html_cache::{
    HtmlCacheConfig, HtmlCacheMiddleware, HtmlPageCache,
};
use poem::{get, handler, EndpointExt, Route};
use std::sync::Arc;

#[handler]
async fn render_article(/* db から記事を取って HTML を返す */) -> String {
    /* 重いレンダリング */ String::new()
}

let cache = Arc::new(HtmlPageCache::new(HtmlCacheConfig::from_env()));

let app = Route::new()
    .at("/article/:id", get(render_article))
    .data(Arc::clone(&cache))          // 更新ハンドラから purge するため
    .with(HtmlCacheMiddleware(cache)); // ← これだけで AI キャッシュ有効
```

記事更新時は `cache.purge("/article/123").await;` を 1 行呼ぶだけ。
purge は AI の学習信号にもなり、更新の多いページは TTL が自動短縮されます。
個人化部分（ようこそ○○さん等）は `Cookie` 付きリクエストが自動バイパス
されるため、共通 HTML + JS Fetch の分離パターンでどうぞ。

### 4.3 単一 DB で始めて後から DUAL に育てる

```rust
use open_runo_db::dual::DualBackend;
use open_runo_router::state::AppState;

// 開発: SQLite 1 台でも DUAL と同じコードパス
let state = AppState::with_single_db(Arc::new(sqlite_backend));

// 本番: PostgreSQL + aruaru-db の二重化（自動整合性修復つき）
let dual = DualBackend::with_default_routing(postgres, aruaru);
let state = AppState::with_db(Arc::new(dual));
```

### 4.4 社内の分散 DB を 1 つに統合（Federated）

```rust
use open_runo_db::federated::FederatedBackend;

let fed = FederatedBackend::builder()
    .member("tokyo-pg", tokyo_postgres)
    .member("osaka-my", osaka_mysql)
    .route("orders", "osaka-my")
    .broadcast("schemas")
    .default_member("tokyo-pg")
    .build()?;
let state = AppState::with_db(Arc::new(fed));
```

段階的な片寄せは `open_runo_db::migrate::transfer_verified(src, dst, tables)`。

## 5. データのお引越し（既存環境から）

```bash
# 旧環境: 全 DATA + AI 学習記録をポータブル JSON へ（二か所に書き込み）
curl -X POST -H "x-api-key: $KEY" http://old:8080/api/backup/export

# 新環境: ファイル指定で取り込み、または最新を自動発見して一発復活
curl -X POST -H "x-api-key: $KEY" -d '{"path":"..."}' http://new:8080/api/backup/import
curl -X POST -H "x-api-key: $KEY" http://new:8080/api/backup/restore-latest
```

異種エンジンへの変換・Snowflake 取り込み・無停止統合は `docs/migration.md` 参照。

## 6. 環境変数 全一覧

| 変数 | 既定 | 説明 |
|------|------|------|
| `OPEN_RUNO_BIND_ADDR` ほか Config 系 | — | `open-runo-core::Config` 参照 |
| `OPEN_RUNO_JWT_SECRET` | 無効 | HS256 Bearer 認証 |
| `OPEN_RUNO_OIDC_ISSUER` / `_OIDC_JWKS_FILE` / `_OIDC_AUDIENCE` | 無効 | OIDC SSO (JWKS/RS256) |
| `OPEN_RUNO_RBAC` | off | `enforce` でルート単位 RBAC |
| `OPEN_RUNO_SCIM_TOKEN` | 無効 | IdP 用固定 Bearer（/scim/* 限定） |
| `OPEN_RUNO_KEY_ANOMALY_FACTOR` / `_KEY_WARMUP_REQUESTS` / `_KEY_COOLDOWN_SECS` | 20 / 50 / 300 | KeyGuardian 異常検知 |
| `OPEN_RUNO_PQ_MODE` | allow | disabled / allow (APQ) / enforce (Trusted Documents) |
| `OPEN_RUNO_CACHE` / `_CACHE_TTL_SECS` | off / 30 | GraphQL レスポンスキャッシュ |
| `OPEN_RUNO_HTML_CACHE` | off | AI HTML ページキャッシュ（`on` で有効） |
| `OPEN_RUNO_HTML_CACHE_TTL_SECS` / `_MAX_ENTRIES` / `_MIN_HITS` | 60 / 10000 / 2 | 同上の基本設定 |
| `OPEN_RUNO_HTML_CACHE_AI` | on | `off` で固定 min-hits 判定へ |
| `OPEN_RUNO_HTML_CACHE_REFRESH_RATIO` | 0.8 | 先回り再生成の閾値 |
| `OPEN_RUNO_CACHE_AI_MIN_TTL_SECS` / `_MAX_TTL_SECS` / `_DEFAULT_TTL_SECS` / `_MAX_TRACKED` / `_MIN_EXPECTED_HITS` | 5 / 3600 / 60 / 50000 / 1.0 | AI 予測器チューニング |
| `OPEN_RUNO_AI_PERSIST_SECS` | 300 | AI 学習の自動保存（0=off） |
| `OPEN_RUNO_INTEGRITY_SECS` | 3600 | 両 DB 整合性の自動検証・修復（0=off） |
| `OPEN_RUNO_BACKUP_DIR` | ./backups | 一次バックアップ先 |
| `OPEN_RUNO_BACKUP_MIRROR_DIR` | 無効 | 二次（Google Drive 同期フォルダ推奨） |
| `OPEN_RUNO_BACKUP_SECS` | 0 | 定期バックアップ間隔（0=手動） |

## 7. REST サーフェス早見表

| Path | 用途 |
|------|------|
| `/health` `/healthz` | 認証不要ヘルスチェック |
| `/api/openapi.json` | REST APIのOpenAPI 3.0仕様(認証不要、Postman/Insomnia/Swagger UIへインポート可) |
| `/.well-known/acme-challenge/:token` | ACME HTTP-01チャレンジ応答(認証不要、CAが直接アクセスする) |
| `/api/keys/self-issue` | **認証不要**でAPIキーを自動発行(developer role、24時間有効)。人間がキーを意識しない設計の起点 |
| `/mcp` | MCP(Model Context Protocol)Streamable HTTP transport(認証不要、JSON-RPC 2.0)。`initialize`/`tools/list`/`tools/call`に対応、`health_check`・`self_issue_api_key`の2ツールを公開 |
| `/api/session/login` `/logout` | `X-Api-Key`を追加のセッションCookie(HttpOnly+SameSite=Strict)+CSRFトークンへ交換(置き換えではなく追加の認証経路) |
| `/graphql` (+`/graphql/ws`) | Federation GraphQL / Subscriptions(GraphiQLは`GET /graphql`) |
| `/api/schemas*`（`?namespace=`） | Schema Registry（マルチグラフ対応） |
| `/api/schemas/upload` | `multipart/form-data`でSDLファイルを直接アップロードして登録(`/api/schemas`のJSON版と同じ登録処理を共有) |
| `/api/federation/*` | 合成・状態 |
| `/api/persisted-queries*` | Trusted Documents 登録・取得 |
| `/api/db/*` | DUAL DATABASE KV |
| `/api/ai/route` | AI プロバイダ選択 |
| `/api/events` | SSE |
| `/api/ws-echo` | 汎用 WebSocket エコー（手書き RFC 6455、認証不要） |
| `/api/ws-events` | 汎用 WebSocket イベント配信（認証必須、SSE と同じブローカー） |
| `/api/feature-flags*` `/api/feature-flags/:name` `/api/feature-flags/:name/evaluate` | Feature Flags（upsert/list/get/delete/evaluate、決定的バケッティングによる canary ロールアウト） |
| `/scim/v2/Users` `/scim/v2/Groups` | SCIM 2.0（鍵の自動発行/失効つき） |
| `/api/cache/purge` `/purge-all` `/ai-stats` | HTML キャッシュ管理・AI 観測 |
| `/api/backup/export` `/import` `/restore-latest` | バックアップ・復活 |
| `/api/migrate/export-sql` `/export-csv` | エンジン変換エクスポート |
| `/api/integrity/check` | 両 DB 整合性チェック・自動修復 |
| `/api/analytics/requests-per-month` | 月間リクエスト数(運用メトリクスのみ、課金/レート制限には不使用) |
| `/api/analytics/operations` | オペレーション別(method+path)レイテンシ・エラー率集計 |

Federation合成（`POST /api/federation/compose`）は`sdl`フィールドで生の
GraphQL SDL（Federation v1の暗黙ディレクティブ方式・v2の`@link`方式
どちらも自動判別）を渡せます。従来の`{service_name, types: {...}}`
JSON直接指定も後方互換のためそのまま利用可能です。

認証: `X-Api-Key`（KeyGuardian 台帳が空なら開発モードで任意値可）/
JWT / OIDC Bearer / セッションCookie(`/api/session/login`発行、状態変更
リクエストは`X-CSRF-Token`ヘッダ必須)。RBAC 有効時、管理系は `admin`
ロール必須。

TLS を直接終端したい場合（リバースプロキシを使わない構成）は
`tls` Cargo feature（既定オフ）で `hyper_compat::tls::{load_tls_config,
serve_tls}` が使えます。証明書を自動取得したい場合は`acme` feature
（`tls`を暗黙有効化)で`acme::{AcmeClient, obtain_certificate_http01}`
（RFC 8555、HTTP-01のみ）が使えます。gRPCが必要な場合は`grpc::serve_grpc`
（新規依存無し、`grpc.health.v1.Health/Check`）を`OPEN_RUNO_GRPC_BIND_ADDR`
で有効化。ネイティブのシステムトレイ常駐アプリ・OSネイティブ通知・
Windows インストーラーが必要な場合は `apps/desktop-tray`
（`tauri`パッケージ非依存）をそのまま移植できます。

## 8. 動作確認

```bash
cd open-runo
cargo test --workspace     # 302 テスト（--all-features で311）+ doctest が全部通れば OK
cargo run -p open-runo-gateway   # REST + GraphQL 統合バイナリ起動
```

## 9. 命名規約（お引越し先でも守ること）

- クレート/ディレクトリ: `open-runo-*`　- Rust パス: `open_runo_*`
- 環境変数: `OPEN_RUNO_*`　- 型名のみ CamelCase: `OpenRuno*`（Rust 言語制約）

## 10. 詳細ドキュメント

`docs/HANDOFF.md`（全開発履歴）/ `architecture.md` / `cosmo-parity.md` /
`migration.md` / `api-spec.md` / `database.md` / `security.md` / `federation.md`

## 11. 移植・拡張時の注意

他プロジェクトへ移植・拡張する際、依存クレートの現状（バージョン・
メンテナンス状況）や仕様の詳細で判断に迷う場合は、学習データからの
推測だけに頼らず、実際に検索（日本語・英語の両方の Google 検索、
および GitHub 上の実装状況調査）して裏付けを取ってから技術選定を
行うことを推奨する。
