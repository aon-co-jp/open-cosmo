# PORTING.md — open-cosmo お引越しファイル

> このファイル 1 枚で、**どのプロジェクトでも open-cosmo を導入・移設**できます。
> open-e-gov / OpenRedmine / OpenWordPress など新プロジェクトのリポジトリに
> このファイルをコピーして、上から順に進めてください。
>
> 対象バージョン: workspace 0.1.0（18 クレート / 262 テスト、2026-07-11
> 実測。最新のクレート数・テスト数は `CLAUDE.md` の「現状」節参照）
> 最終更新: 2026-07-11

---

## 0. open-cosmo と poem-cosmo-tauri の違い(2026-07-11、ユーザーによる最終定義)

このリポジトリ `open-cosmo` は、姉妹リポジトリ `poem-cosmo-tauri`
(https://github.com/aon-co-jp/poem-cosmo-tauri) と共通コア(下記「cosmo
部分」)を持つが、**全く違うリポジトリのプロジェクト**であり統合対象では
ない。poem-cosmo-tauri はこの共通コアに加えて、(1) Rust の Poem
フレームワークの全機能を完全互換で一から自作・再現したバックエンド、
(2) Tauri の全機能を完全互換で一から自作・再現したフロントエンド、を
持ち、その2つとcosmoコアの3者をスムーズに連携させることで効率的な
WEB開発を実現するフレームワーク/ミドルウェアという、より広いスコープを
持つ。**このリポジトリ(open-cosmo)にその上乗せスコープはなく、以下の
cosmo部分が中心**。詳細は `CLAUDE.md` の該当節、または open-raid-z の
`CLAUDE.md`(正本)を参照。

## 1. open-cosmo とは（cosmo部分、30 秒版）

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
open-cosmo/
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
open-runo-core     = { path = "../open-cosmo/crates/open-runo-core" }
open-runo-router   = { path = "../open-cosmo/crates/open-runo-router" }
open-runo-gateway  = { path = "../open-cosmo/crates/open-runo-gateway" }
open-runo-db       = { path = "../open-cosmo/crates/open-runo-db" }
open-runo-cache    = { path = "../open-cosmo/crates/open-runo-cache" }
open-runo-security = { path = "../open-cosmo/crates/open-runo-security" }

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
| `/mcp` | MCP(Model Context Protocol)Streamable HTTP transport(認証不要、JSON-RPC 2.0)。`initialize`/`tools/list`/`tools/call`/`resources/list`/`resources/read`/`prompts/list`/`prompts/get`に対応。実ツール2種(`health_check`・`self_issue_api_key`)+実リソース2種(`openapi://spec`・`health://status`)+実プロンプト1種(`summarize_api`、同じ`openapi::spec()`を動的にレンダリング)、いずれも既存REST APIと同じ本番ロジックを共有 |
| `/api/session/login` `/logout` | `X-Api-Key`を追加のセッションCookie(HttpOnly+SameSite=Strict)+CSRFトークンへ交換(置き換えではなく追加の認証経路) |
| `/graphql` (+`/graphql/ws`) | Federation GraphQL / Subscriptions(GraphiQLは`GET /graphql`) |
| `/api/schemas*`（`?namespace=`） | Schema Registry（マルチグラフ対応） |
| `/api/schemas/upload` | `multipart/form-data`でSDLファイルを直接アップロードして登録(`/api/schemas`のJSON版と同じ登録処理を共有) |
| `/api/federation/*` | 合成・状態 |
| `/api/persisted-queries*` | Trusted Documents 登録・取得 |
| `/api/db/*` | DUAL DATABASE KV |
| `/api/db/:table/:key/at/:commit_id` | VersionLessAPI + Git-on-SQL ハイブリッドの読み出し側 — 指定コミット時点の値を取得(`aruaru-db`バックエンドのみ実対応、他バックエンドは501) |
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
（`tls`を暗黙有効化）で`acme::{AcmeClient, obtain_certificate_http01,
tls_alpn01::obtain_certificate, dns01::obtain_certificate}`（RFC 8555、
HTTP-01・TLS-ALPN-01・DNS-01の3チャレンジ型すべてに対応）が使えます。
DNS-01は`acme::dns01::DnsProvider` traitを実装すれば任意のDNSプロバイダに
対応可能（付属の`CloudflareDnsProvider`はbearerトークンのみ・署名不要な
最小実装例）。gRPCが必要な場合は`grpc::serve_grpc`（新規依存無し、
`grpc.health.v1.Health/Check`・`/Watch`ストリーミング・
`grpc.reflection.v1.ServerReflection`の`list_services`）を
`OPEN_RUNO_GRPC_BIND_ADDR`で有効化。ネイティブのシステムトレイ常駐アプリ・
OSネイティブ通知・Windows インストーラーが必要な場合は `apps/desktop-tray`
（`tauri`パッケージ非依存）をそのまま移植できます。

## 8. 動作確認

```bash
cd open-cosmo
cargo test --workspace     # 302 テスト（--all-features で311）+ doctest が全部通れば OK
cargo run -p open-runo-gateway   # REST + GraphQL 統合バイナリ起動
```

## 9. 命名規約（お引越し先でも守ること）

- クレート/ディレクトリ: `open-runo-*`　- Rust パス: `open_runo_*`
- 環境変数: `OPEN_RUNO_*`　- 型名のみ CamelCase: `OpenRuno*`（Rust 言語制約）

## 10. 詳細ドキュメント

`docs/HANDOFF.md`（全開発履歴）/ `architecture.md` / `cosmo-parity.md` /
`migration.md` / `api-spec.md` / `database.md` / `security.md` / `federation.md`
`deployment-scaling.md`(複数インスタンス+nginx/Caddy等リバースプロキシ配下での水平スケーリングレシピ、graceful shutdown・実ヘルスチェックの実装根拠、2026-07-13)

## 11. 移植・拡張時の注意

他プロジェクトへ移植・拡張する際、依存クレートの現状（バージョン・
メンテナンス状況）や仕様の詳細で判断に迷う場合は、学習データからの
推測だけに頼らず、実際に検索（日本語・英語の両方の Google 検索、
および GitHub 上の実装状況調査）して裏付けを取ってから技術選定を
行うことを推奨する。

**パフォーマンス・並行処理**: 移植先でも、非同期（tokio マルチスレッド
ランタイム）を基本としつつ、必要な場面のみ同期処理を用いる方針を
踏襲すること。`#[tokio::main]` のランタイム flavor が
`current_thread` に固定されていないか、async 関数内でブロッキング
I/O・CPU 負荷処理を直接呼んでいないか（`tokio::task::spawn_blocking`
へ退避すべき）、CPU 律速な処理は `rayon` 等でのデータ並列化を検討する
価値があるか、を移植時にも確認するとよい。

## 12. RPoem ⇔ open-cosmo の「Cosmo共通コア」重複整理(2026-07-23)

RPoemは`docs/HANDOFF.md`の通りopen-cosmoと共通のCosmoコア(Federation・
VersionlessAPI・SCIM・Security・Cache等)を持つ姉妹リポジトリ。実際に
両リポジトリの`crates/`配下をファイル単位で`diff`した結果を記録する
(詳細な調査過程はRPoem側`PORTING.md`同節に記録、本節は要点のみ)。

- **調査結果**: 共通20クレートのうち **18クレートが両リポジトリで
  byte-for-byte完全一致**(共通コア全体)。残る`open-runo-db`/
  `open-runo-gateway`/`open-runo-router`の3クレートは、RPoem固有の
  追加(`appserver_tenants.rs`・`udp_notice.rs`・gRPC/ACME/MCP拡張等)
  により意図的に分岐している——これは正常(RPoem固有スコープの範囲)。
- **既存パターンの踏襲**: 「別リポジトリのcrateへ直接のCargo依存は
  しない、小さなモジュールとして直接コピーする」という既存方針
  (RustJSON移植等)を継続する判断。ワークスペース統合はスコープが
  異なる別プロジェクトという既存方針に反するため不採用。
- **新規: `scripts/sync-cosmo-core.sh`**(RPoem/open-cosmo両方に同一ファイル
  配置)。従来手作業だった「ミラー」を機械的に検証・実行できるように
  した(`check`/`diff <crate>`/`push <crate>`/`pull <crate>`のサブ
  コマンド、詳細はスクリプト自体のコメントを参照)。今回、RPoem側で
  発見・修正した`open-runo-appserver`のWindows非互換テストバグ
  (`supervisor_reports_up_for_long_running_process_and_stops_it`が
  `sleep`コマンドをハードコードしておりWindows環境で常に失敗していた
  実バグ、`cfg!(windows)`で`ping -n 30 127.0.0.1`へ分岐して修正)を
  このスクリプトの`push`でopen-cosmo側へ同期し、`check`で0件drift
  (18/18 in sync)を確認済み——このリポジトリ側の`CLAUDE.md`
  「2026-07-22」エントリが「ネイティブWindows cargoでこの2テストが
  失敗する」と記録していた既知issueのうち1件を、RPoem側の修正が
  そのまま解消した形になる(もう1件の並行アクセステストの不安定性は
  環境依存の一過性issueと判断され、コード変更は行っていない——
  詳細はRPoem側`CLAUDE.md`のHANDOFF追記を参照)。

## API 方針: REST 全廃・gRPC / Connect へ (2026-11-04)

### 日本語
- **REST は全廃する。新しい API は REST で作らない。** ブラウザや `curl` から呼ぶ API は **Connect の単純呼び出し**(`POST /<パッケージ>.<Service>/<Method>`、`content-type: application/json`、本文は JSON)、サービス間は **gRPC / gRPC-web**(RPoem の管理API `open_runo.admin.v1.Admin` と同じ流儀)。ストリーミングは、必要になってから。
- **既存の REST は、触るときに順に移す。** 移したら旧 REST の経路は消す。移行が終わるまで残るが、**新規には追加しない**。
- **JSON は RS-JSON に統一**する(読み込みは `from_str_secure` / `parse_secure` でサイズ・深さ・重複キーの上限つき、書き出しは `rust_json::fast`)。ブラウザの WASM に `prost` は入れない(サイズが増えるため)。
- **例外(自分たちの API ではなく、相手の規格が決めているもの)**: ACME、OAuth/OIDC のコールバック、第三者(GitHub など)からの Webhook、ヘルスチェック、静的ファイル配信、OTLP など。これらは規格どおりの HTTP を受ける。
- 参考実装: `RPoem`(`open_runo.admin.v1.Admin`)、`open-easy-web`(`server/src/connect.rs`、`proto/open_easy_web/admin/v1/admin.proto`)。
- ※ このリポジトリのコードが既に方針どおりかどうかは、各リポジトリの現状による。この節は方針の記録であり、コードの変更を意味しない。

### English
- **REST is being abolished. New APIs are not built as REST.** APIs called from browsers or `curl` use **Connect unary calls** (`POST /<package>.<Service>/<Method>`, `content-type: application/json`, JSON body); service-to-service calls use **gRPC / gRPC-web** (the same style as RPoem's admin API `open_runo.admin.v1.Admin`). Streaming only when it is actually needed.
- **Existing REST endpoints are migrated one by one, when they are touched.** The old REST route is removed once migrated. Until migration is finished they remain, but **no new ones are added**.
- **JSON is unified on RS-JSON**: reading with `from_str_secure` / `parse_secure` (size, depth and duplicate-key limits), writing with `rust_json::fast`. `prost` is not put into browser WASM (it would grow the binary).
- **Exceptions (shapes dictated by someone else's standard, not our own API)**: ACME, OAuth/OIDC callbacks, webhooks from third parties (e.g. GitHub), health checks, static file serving, OTLP, and so on. These accept plain HTTP as the standard requires.
- Reference implementations: `RPoem` (`open_runo.admin.v1.Admin`) and `open-easy-web` (`server/src/connect.rs`, `proto/open_easy_web/admin/v1/admin.proto`).
- Note: whether the code in a given repository already follows this policy depends on that repository. This section records the policy; it does not mean the code has been changed.

### Deutsch (KI-Übersetzung, redaktionell überarbeitet; noch nicht von Muttersprachlern geprüft)
- **REST wird vollständig abgeschafft; neue APIs entstehen nicht mehr als REST.** APIs, die aus dem Browser oder per `curl` aufgerufen werden, verwenden **unäre Connect-Aufrufe** (`POST /<Paket>.<Service>/<Methode>`, `content-type: application/json`, JSON-Body). Zwischen Diensten kommen **gRPC bzw. gRPC-web** zum Einsatz, im selben Stil wie die Admin-API von RPoem (`open_runo.admin.v1.Admin`). Streaming wird erst ergänzt, wenn es tatsächlich gebraucht wird.
- **Bestehende REST-Endpunkte werden schrittweise umgestellt, und zwar immer dann, wenn man ohnehin an ihnen arbeitet.** Sobald ein Endpunkt umgestellt ist, wird die alte REST-Route entfernt. Bis zum Abschluss der Migration bleiben die übrigen bestehen, es werden aber **keine neuen mehr hinzugefügt**.
- **JSON wird einheitlich über RS-JSON verarbeitet**: Gelesen wird mit `from_str_secure` bzw. `parse_secure` (mit Obergrenzen für Größe, Verschachtelungstiefe und doppelte Schlüssel), geschrieben mit `rust_json::fast`. `prost` wird nicht in das Browser-WASM aufgenommen, da es die Datei vergrößern würde.
- **Ausnahmen (Schnittstellen, deren Form ein fremder Standard vorgibt und die nicht zu unserer eigenen API gehören)**: ACME, OAuth/OIDC-Callbacks, Webhooks von Drittanbietern (z. B. GitHub), Health-Checks, die Auslieferung statischer Dateien, OTLP und Ähnliches. Diese nehmen, wie vom jeweiligen Standard verlangt, gewöhnliches HTTP entgegen.
- Referenzimplementierungen: `RPoem` (`open_runo.admin.v1.Admin`) und `open-easy-web` (`server/src/connect.rs`, `proto/open_easy_web/admin/v1/admin.proto`).
- Hinweis: Ob der Code eines einzelnen Repositorys diese Richtlinie bereits erfüllt, hängt vom jeweiligen Repository ab. Dieser Abschnitt dokumentiert lediglich die Richtlinie; er besagt nicht, dass der Code schon angepasst wurde.

### Русский (перевод выполнен с помощью ИИ и отредактирован; носителями языка пока не проверен)
- **От REST мы полностью отказываемся; новые API больше не создаются в стиле REST.** API, к которым обращаются из браузера или через `curl`, используют **унарные вызовы Connect** (`POST /<пакет>.<Service>/<Method>`, `content-type: application/json`, тело запроса — JSON). Для взаимодействия между сервисами применяются **gRPC и gRPC-web** — в том же стиле, что и административный API RPoem (`open_runo.admin.v1.Admin`). Потоковая передача данных добавляется лишь тогда, когда в ней действительно возникает необходимость.
- **Существующие REST-эндпоинты переводятся поэтапно — по мере того, как с ними приходится работать.** После перевода старый REST-маршрут удаляется. Пока миграция не завершена, оставшиеся эндпоинты сохраняются, но **новые не добавляются**.
- **Работа с JSON унифицирована на основе RS-JSON**: чтение — через `from_str_secure` или `parse_secure` (с ограничениями на размер, глубину вложенности и повторяющиеся ключи), запись — через `rust_json::fast`. `prost` в браузерный WASM не включается, поскольку это увеличило бы размер файла.
- **Исключения (интерфейсы, форму которых определяет чужой стандарт, а не наш собственный API)**: ACME, обратные вызовы OAuth/OIDC, вебхуки сторонних сервисов (например, GitHub), проверки работоспособности, раздача статических файлов, OTLP и тому подобное. Такие интерфейсы принимают обычный HTTP, как того требует соответствующий стандарт.
- Эталонные реализации: `RPoem` (`open_runo.admin.v1.Admin`) и `open-easy-web` (`server/src/connect.rs`, `proto/open_easy_web/admin/v1/admin.proto`).
- Примечание: соответствует ли код конкретного репозитория этой политике, зависит от самого репозитория. Данный раздел лишь фиксирует политику и не означает, что код уже изменён.

## API 方針の補足: 用語の整理と使い分け (2026-11-04)

### 日本語
**1つに統一するものではなく、別の層の話です。** 「REST の代わり」になるのは Connect・gRPC・GraphQL だけで、WASM は画面を作る技術です。

| 用語 | 何か | REST の代わりか |
|---|---|---|
| REST | URL と HTTP メソッドで操作を表す API の作り方 | 全廃する側 |
| **Connect** | `POST /サービス名/メソッド名` に JSON を送る形式。`curl` でもブラウザの `fetch` でも呼べる | はい(**採用**) |
| **gRPC** | 同じ「サービス/メソッド」の考え方を、バイナリ(protobuf)と HTTP/2 で行う形式。速く、ストリーミングも得意 | はい(**サービス同士に採用**) |
| gRPC-web | gRPC のブラウザ向けの変形版 | はい(必要なら) |
| GraphQL | 欲しい項目を問い合わせる形式 | 一部だけ(**問い合わせ(読み取り)専用**) |
| WASM | Rust をブラウザの中で動かす仕組み | いいえ(画面を作る技術) |

- **ブラウザの画面や `curl` から呼ぶ API は Connect**(POST、JSON を RS-JSON で読み書き)。ブラウザの WASM は素の gRPC を話せず、Connect なら `fetch` のままで、`prost` を足さずに済む(画面のサイズも増えない)。
- **サービス同士の通信(サーバー間、重い処理、ストリーミング)は gRPC。** 多くは Connect で足りるので、ストリーミングが必要になった所だけ gRPC に上げる。
- **GraphQL は、まとめて問い合わせたいときの読み取り専用。** 書き込み(操作)には使わない。
- **WASM は画面を作る技術として、そのまま使う。** 画面から呼ぶ API が Connect になっただけ。
- 外部の規格が形を決めるもの(ACME、OAuth のコールバック、第三者の Webhook、ヘルスチェック、静的ファイル、OTLP)は、規格どおりの HTTP のまま。
- **迷ったら Connect。**

### English
**These are not alternatives to unify; they sit at different layers.** Only Connect, gRPC and GraphQL can replace REST; WASM is a way to build the UI.

| Term | What it is | Replaces REST? |
|---|---|---|
| REST | API style that expresses operations through URLs and HTTP methods | The thing being abolished |
| **Connect** | `POST /Service/Method` with a JSON body; callable from `curl` and from a browser `fetch` | Yes (**adopted**) |
| **gRPC** | The same service/method idea over binary (protobuf) and HTTP/2; fast, good at streaming | Yes (**for service-to-service**) |
| gRPC-web | A browser-friendly variant of gRPC | Yes (if needed) |
| GraphQL | A query language: ask for exactly the fields you want | Partly (**read-only queries**) |
| WASM | Runs Rust inside the browser | No (a UI technology) |

- **APIs called from browser screens or `curl` use Connect** (POST, JSON read and written with RS-JSON). Browser WASM cannot speak plain gRPC; with Connect it stays a `fetch`, and `prost` is not needed (the binary does not grow).
- **Service-to-service calls (server to server, heavy work, streaming) use gRPC.** Connect is enough in most places; move a call to gRPC only where streaming is actually needed.
- **GraphQL is read-only**, for queries that gather several pieces of data. It is not used for writes (operations).
- **WASM stays as the way to build the UI.** The API it calls simply became Connect.
- Interfaces whose shape an outside standard dictates (ACME, OAuth callbacks, third-party webhooks, health checks, static files, OTLP) stay plain HTTP.
- **When in doubt, use Connect.**

### Deutsch (KI-Übersetzung, sprachlich überarbeitet; noch nicht von Muttersprachlern geprüft)
**Es geht nicht darum, eine einzige Technik auszuwählen – die Begriffe liegen auf unterschiedlichen Ebenen.** Als Ersatz für REST kommen nur Connect, gRPC und GraphQL in Frage; WASM dagegen ist eine Technik, mit der die Benutzeroberfläche gebaut wird.

| Begriff | Was ist das? | Ersatz für REST? |
|---|---|---|
| REST | Ein API-Stil, bei dem Operationen über URLs und HTTP-Methoden ausgedrückt werden | wird abgeschafft |
| **Connect** | `POST /Dienst/Methode` mit JSON im Body; aufrufbar mit `curl` ebenso wie mit `fetch` im Browser | ja (**gewählt**) |
| **gRPC** | Dasselbe Prinzip aus Dienst und Methode, aber über binäres protobuf und HTTP/2; schnell und gut für Streaming geeignet | ja (**zwischen Diensten**) |
| gRPC-web | Eine für den Browser angepasste Variante von gRPC | ja (bei Bedarf) |
| GraphQL | Abfragesprache, mit der man genau die benötigten Felder anfordert | teilweise (**nur für lesende Abfragen**) |
| WASM | Führt Rust-Code direkt im Browser aus | nein (Technik für die Oberfläche) |

- **APIs, die von Browseroberflächen oder per `curl` aufgerufen werden, verwenden Connect** (POST; JSON wird mit RS-JSON gelesen und geschrieben). WASM im Browser kann kein reines gRPC sprechen. Mit Connect bleibt es bei einem gewöhnlichen `fetch`, und `prost` ist nicht nötig – die WASM-Datei wird dadurch nicht größer.
- **Für die Kommunikation zwischen Diensten (Server zu Server, rechenintensive Aufgaben, Streaming) wird gRPC eingesetzt.** In den meisten Fällen reicht Connect aus; auf gRPC wird nur dort umgestellt, wo Streaming tatsächlich gebraucht wird.
- **GraphQL dient ausschließlich lesenden Abfragen**, wenn mehrere Informationen gebündelt abgefragt werden sollen. Für Schreibzugriffe (Operationen) wird es nicht verwendet.
- **WASM bleibt die Technik für die Oberfläche.** Geändert hat sich lediglich, dass die aufgerufene API nun auf Connect beruht.
- Schnittstellen, deren Form durch einen externen Standard festgelegt ist (ACME, OAuth-Callbacks, Webhooks von Drittanbietern, Health-Checks, statische Dateien, OTLP), bleiben gewöhnliches HTTP.
- **Im Zweifel: Connect.**

### Русский (перевод выполнен с помощью ИИ и литературно отредактирован; носителями языка пока не проверен)
**Это не варианты, из которых нужно выбрать один, а технологии разных уровней.** Заменить REST могут только Connect, gRPC и GraphQL; WASM же — это способ создания пользовательского интерфейса.

| Термин | Что это | Заменяет REST? |
|---|---|---|
| REST | Стиль построения API, при котором операции выражаются через URL и HTTP-методы | отменяется |
| **Connect** | `POST /Сервис/Метод` с телом в формате JSON; вызывается как из `curl`, так и из браузерного `fetch` | да (**выбран**) |
| **gRPC** | Тот же принцип «сервис — метод», но поверх двоичного формата protobuf и HTTP/2; работает быстро и хорошо подходит для потоковой передачи | да (**для связи между сервисами**) |
| gRPC-web | Адаптированный для браузера вариант gRPC | да (при необходимости) |
| GraphQL | Язык запросов: клиент запрашивает ровно те поля, которые ему нужны | частично (**только запросы на чтение**) |
| WASM | Позволяет выполнять код на Rust прямо в браузере | нет (технология для интерфейса) |

- **API, которые вызываются из браузерных интерфейсов или через `curl`, строятся на Connect** (POST; JSON читается и записывается через RS-JSON). Браузерный WASM не умеет работать с «чистым» gRPC. С Connect достаточно обычного `fetch`, а `prost` не требуется — размер WASM-файла не увеличивается.
- **Для обмена между сервисами (сервер — сервер, ресурсоёмкие задачи, потоковая передача) применяется gRPC.** В большинстве случаев достаточно Connect; на gRPC переходят только там, где потоковая передача действительно необходима.
- **GraphQL используется только для чтения**, когда нужно получить сразу несколько связанных данных. Для записи (операций) он не применяется.
- **WASM по-прежнему остаётся технологией для интерфейса.** Изменилось лишь то, что вызываемый им API теперь построен на Connect.
- Интерфейсы, форма которых задана внешним стандартом (ACME, обратные вызовы OAuth, вебхуки сторонних сервисов, проверки работоспособности, статические файлы, OTLP), остаются обычным HTTP.
- **Если есть сомнения — выбирайте Connect.**

## REST 全廃 → RPoem の gRPC / Connect 構成へ(2026-11-05)

管理操作は **RPoem と同じ `open_runo.admin.v1.Admin`**(gRPC。同じ定義を gRPC-web・Connect でも受け、`gateway` の `POST /open_runo.admin.v1.Admin/<Method>` が Content-Type で振り分ける)。旧 `/api/*` の REST は無く 404。エラーは `{"code","message"}`。定義の正は `crates/open-runo-proto/proto/open_runo/admin/v1/admin.proto`、処理本体は `open-runo-router` の `admin_ops.rs`/`admin_sys.rs`。詳細・経緯は RPoem の `CLAUDE.md` 冒頭「方針転換(gRPC)」と `docs/connect-hybrid.md`。

- **経緯**: 2026-11-04 に自前の「外側 Connect アダプター」(`open_cosmo.admin.v1.Admin`、`connect.rs`)で移したが、RPoem(実装の先行地点)の gRPC / Connect 構成と二重になるため、2026-11-05 に **RPoem の構成へ置き換えた**(自前アダプターは撤去)。
- **取り込み方**: RPoem の該当クレート(router・gateway・proto・connect-core/server・jwt・openapi ほか、計26)と `apps/desktop-wasm`・`desktop-tray` をそのまま持ち込み、ワークスペースを合わせた(CLI・管理画面も Connect/gRPC-web 版)。
- **open-cosmo だけの差(意図的)**: 兄弟リポジトリへの path 依存を持たない方針のため、RPoem の `open-web-server-wire` を使う UDP 通知リスナー(`udp_notice.rs`)は **無効化**(カウンタ型と `spawn_from_env`=常に `None` だけ残した)。`tests/ledger_fusion.rs`(open-web-server との融合テスト)も除いた。UDP 通知が要るなら RPoem 側のルーターを使う。
- **例外(移さない)**: `/health`・ACME・`/mcp`・SCIM・`/graphql`(Cosmo 互換)・静的ファイル・Webhook など、相手の規格が形を決めるもの。
- **確認**: `cargo test --workspace` 全通過(appserver のテストは先に `cargo build -p open-runo-appserver --bins` が要る)。gateway 実バイナリで、旧REST 404・認証なし401・不正JSON400・不明メソッド501・DbStatus 200 を確認。`apps/desktop-wasm` は wasm32 の `cargo check` まで。
- **重複キー拒否(open-cosmo で追加)**: `open-runo-connect-server` の JSON 入口で、proto3-JSON へ変換する前に RS-JSON の `parse_secure`(8 MiB・深さ64・重複キー拒否)で検査する(`rust-json` を git 依存)。テスト追加(`tests.rs`)。**RPoem 側には未反映**(RPoem の connect-server に同じ変更を入れて同期するのが筋)。
- **実機確認(2026-11-05)**: gateway 実バイナリに curl — 旧REST 404 / 認証なし401 / 不正JSON400 / 重複キー400 / 正常200。ブラウザ(WASM 新ビルド)で 10 ページすべて表示(白画面・エラーなし)、Db の Put→Get→Delete→Get(404)・フラグ保存が Connect で成功。落とし穴: 以前のビルドが**ブラウザにキャッシュ**されていると旧 `open_cosmo.admin.v1.Admin` を呼んで 404 になる(`www/pkg` は gitignore の生成物。`make wasm-frontend` で再生成し、強制再読み込み)。
- **未了**: VPS では open-cosmo は稼働していない(置かない、ユーザー判断)。
- `docs/*.md`・`README-*.md` の旧 `/api/...` の記述は順次更新。
