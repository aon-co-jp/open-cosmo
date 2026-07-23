# RCosmo — 新セッション引き継ぎ文書

> **作成日**: 2026-07-03  
> **目的**: 新しい Cowork / Claude セッションへのプロジェクト状態の完全引き継ぎ

---

## -1. 自律メンテナンスパス（2026-07-10、無人・毎時実行）

- `cargo check --workspace` / `cargo test --workspace --no-run`: 変更前から
  成功（ビルド破損なし、176 テストコンパイル済み）。
- `todo!()` / `unimplemented!()` / TODO / FIXME / フェイクデータを返す
  スタブ関数をリポジトリ全体で検索 → **該当なし**。実装は最新 Phase（F）まで
  完了している。
- README 多言語版（10言語）を精査 → **README-Japan.md と README-English.md
  が Phase A 以前の古いビジョン文書のまま残存**しており、実際の実装内容と
  矛盾していた（「設計・開発初期段階」「License TBD」「外部LLMプロバイダへ
  ルーティング」等、現状の自己学習AI・15クレート・176テストと不一致）。
  → ルート `README.md`（最新・正確）を基準に両ファイルを修正済み。
  他8言語版（中/韓/西/仏/独/伊/露/アラビア語）は内容確認済みで正確、変更不要。
- `CLAUDE.md` は open-raid-z の正本と技術スタック・関連プロジェクト一覧が
  一致していることを確認、HANDOFF 節を追記。
- 次回パスへの引き継ぎ: 緊急課題なし。次点候補は本ファイル §「次セッション
  候補」（Google Drive API 直接統合、FederatedBackend の TOML 設定化、
  個人化パターンのサンプル実装、per-field `@cacheControl` 等）。

---

## 0. セッション更新履歴（2026-07-03 午後）

### 仕様変更（ユーザー決定）

**GraphQL ファースト + Cosmo 有料版互換へ方針変更。**
詳細な機能対応表と実装フェーズは **`docs/cosmo-parity.md`** を参照。
Cosmo の Launch/Scale/Enterprise 限定機能（SSO・SCIM・RBAC・永続クエリ・
キャッシュ制御・細粒度レートリミット）を Rust+Poem で OSS 実装する。
次セッションは Phase A（persisted-queries クレート新設、レートリミット拡張、RBAC 設計）から。

### 今回の実装・修正

1. `DualBackend::single(backend)` + `is_single()` を open-runo-db に追加。
   `AppState::with_single_db()` で router からも利用可（HANDOFF 項目 7 完了）。
2. Tauri コマンド `db_list` / `db_get` / `db_put` を追加、TS 側 `dbList/dbGet/dbPut` も追加（項目 8 完了）。
   さらに TS から呼ばれていたのに Rust 側に無かった `compose_schemas` コマンドを実装（バグ修正）。
3. `docs/architecture.md` に open-aruaru 中心ミドルウェア全体図を追記（項目 6 完了）。
4. **ビルドを壊していたバグ群を修正**（前回状態はコンパイル不能だった）:
   - root Cargo.toml: poem に存在しない `macros` feature 指定 → 削除
   - `open-runo-observability/src/lib.rs`・`open-runo-ai-routing/src/lib.rs`: ファイル末尾がディスク上で欠損 → テスト復元
   - `DatabaseTarget` に `Default` 未実装なのに `RoutingTable` が derive(Default) → 修正
   - 全 handler の `Data<Arc<AppState>>` → poem の正しい `Data<&Arc<AppState>>` 形式へ
   - router(bin) → gateway → router(lib) の循環依存 → **統合バイナリを open-runo-gateway 側へ移動**。
     `cargo run -p open-runo-gateway` = REST + /graphql、`cargo run -p open-runo-router` = REST のみ
   - open-runo-db に dev-dependencies tokio 追加、ai-routing テストの一時値 borrow 修正
   - poem TestClient の `resp.json().await` 型不一致 → `.value().deserialize()` に修正（router/gateway）
   - JWT: jsonwebtoken の leeway 60 秒デフォルトで期限切れ判定テストが失敗 → `leeway = 0` に設定
5. **検証結果**: `cargo check --workspace` 警告 0 で成功。
   `cargo test --workspace --exclude open-runo-gateway` **全 23 スイート成功**。
   open-runo-gateway は `cargo check --tests` 成功（実行テストはサンドボックスの
   45 秒制限内でリンクできず未実行 → Windows 側で `cargo test -p open-runo-gateway` を要実行）。

### Phase A 実装完了（同日・仕様変更後）

1. **`open-runo-persisted-queries` クレート新設**（13 クレート目）。
   SHA-256 Trusted Documents。`EnforcementMode::{Disabled, Allow, Enforce}`。
   Allow は Apollo APQ 互換（hash+query の初回自動登録）、Enforce は登録済みハッシュのみ実行。
   `persisted_queries` テーブルは DUAL ルーティングで `Both`（耐久性）。
2. **`open-runo-security` 拡張**: `TokenBucketLimiter`（per-key トークンバケット、
   `with_override` で API キー別バジェット）。既存 fixed-window `RateLimiter` は互換維持。
3. **`open-runo-security::rbac` 新設**: `Resource` × `Action` 権限マトリクス、
   組み込みロール admin / developer / viewer（Cosmo Studio 準拠）、`Claims.roles` と接続可能。
4. **router 配線**: `POST /api/persisted-queries`（登録）/ `GET /api/persisted-queries/:hash`（取得）。
5. **検証**: `cargo check --workspace` 成功、`cargo test --workspace --lib` **13 スイート全成功**
   （**open-runo-gateway 2 テスト含む — Windows 側での再確認は不要になった**が、
   `cargo test --workspace`（doctest 含むフル）を一度回しておくと安心）。
6. Cargo.lock を RCosmo 直下に生成（バージョン固定）。

### Phase B 実装完了

1. **gateway に Persisted Queries 統合**: `/graphql` が
   `extensions.persistedQuery.sha256Hash` を解釈。
   `OPEN_RUNO_PQ_MODE=disabled|allow|enforce`（既定 allow）。
   Enforce = Trusted Documents。テスト用 `graphql_route_with_mode` 公開。
2. **RBAC 配線**: `ApiKeyAuth::with_rbac` / `OPEN_RUNO_RBAC=enforce`。
   JWT 呼び出しをメソッド+パス → `(Resource, Action)`（`auth::required_permission`）
   で認可、拒否 403。API キーは RBAC 対象外。Claims は request extensions へ。
3. **監査ログ**: `router::audit`。schema.register / db.put / db.delete /
   persisted_query.register を `audit_log`（aruaru-db ルート）に記録。

### Phase B2 実装完了

1. **OIDC SSO**（`open-runo-security::oidc`）: JWKS/RS256 検証（kid・iss・aud・exp、
   leeway 0、alg 混同ガード）。env: `OPEN_RUNO_OIDC_ISSUER` +
   `OPEN_RUNO_OIDC_JWKS_FILE`（+ `OPEN_RUNO_OIDC_AUDIENCE`）。
   `ApiKeyAuth::with_oidc` で HS256 と併用可。テスト鍵:
   `crates/open-runo-security/testdata/`（テスト専用、本番使用禁止）。
2. **SCIM 2.0 Users**（`open-runo-scim`）: RFC 7643/7644、`/scim/v2/Users` CRUD、
   IdP 用固定トークン `OPEN_RUNO_SCIM_TOKEN`（`/scim/*` 限定 Bearer）、
   RBAC 上は `Resource::Admin`、監査ログ記録。

### Phase C 実装完了（15 クレート・132 テスト）

1. **`open-runo-cache` クレート**: `Cache` トレイト + `InMemoryTtlCache`。
   `redis-backend` feature で Redis/KeyDB/DragonflyDB（コンパイル確認済み）。
   **gateway レスポンスキャッシュ**: `OPEN_RUNO_CACHE=on` +
   `OPEN_RUNO_CACHE_TTL_SECS`（既定 30 秒）。query のみ、キーは
   document+variables+operation の SHA-256、エラー応答は非キャッシュ。
2. **GraphQL Subscriptions**: `AppState.events`（broadcast、容量 256）。
   スキーマ登録で `SchemaEvent` 配信、`SubscriptionRoot::schema_events`、
   WebSocket は `GET /graphql/ws`（graphql-ws プロトコル）。
3. **SCIM Groups**: `/scim/v2/Groups` CRUD、`displayName eq "x"` フィルタ、
   PUT でメンバーシップ全置換、監査ログ記録。
4. **マルチグラフ / namespace**: `SchemaRegistry::register_in/latest_in/history_in/
   namespaces/services_in`（既存 API は `default` namespace で完全互換）。
   REST: body `namespace` / `?namespace=`。レスポンスに `namespace` 追加。
5. **検証**: check 警告ゼロ、全 15 スイート **132 テスト成功**。

### Phase D 実装完了（2026-07-04、152 テスト）— AI HTML ページキャッシュ

**「同じ表示を繰り返すページを判定して Rust+Poem 側でキャッシュする」ミドルウェア + 独自 AI**
（外部 LLM・有料契約は一切不要。純 Rust の統計学習のみで、使うほど賢くなる）。

1. **`open-runo-cache::predictor::CachePredictor`（自己学習 AI）**:
   - リクエスト到着間隔の EWMA 学習（key 単位 + URL パターン単位）
   - **パターン汎化** = `/page/123` → `/page/*` として学習し、初見の
     `/page/999` でも初回からキャッシュ判定可能（コールドスタート予測）
   - **コスト学習** = レンダリング所要時間を学習し、重いページほど
     低トラフィックでも積極キャッシュ + TTL 延長（サーバー負荷を優先削減）
   - **適応 TTL** = purge（更新）間隔の EWMA から TTL を自動調整
     （更新頻度不明時は default 60 秒から開始、上下限 env 設定可）
   - snapshot/restore（serde）で学習結果の永続化・観測が可能
2. **HTML ページキャッシュ・ミドルウェア**（`router::middleware::html_cache`）:
   - セーフティファースト: GET のみ / Cookie・Authorization・X-Api-Key で
     bypass / `/api` `/scim` `/graphql` `/health` 除外 /
     `Cache-Control: private|no-store` 検知 / 200 のみ保存
   - URL 正規化（utm_*・fbclid 等除去 + クエリソート）、x-cache HIT/MISS/COLD
   - **Singleflight**（per-key ロック）でサンダリングハード防止
     （テスト: 同時 10 リクエストでレンダリング 1 回を実証）
   - **先回り再生成（SWR）**: TTL の 80% 経過後のヒットで即時返却 +
     裏で自動再レンダリング → ユーザーは MISS を見なくなる
   - `InMemoryTtlCache::with_capacity`（OOM ガード、満杯時は期限最短を追い出し）
   - AI OFF 時は 2-Hit 最小カウントにフォールバック
3. **管理 API**: `POST /api/cache/purge`（記事更新時のピンポイント破棄）/
   `purge-all` / `GET /api/cache/ai-stats`（ヒット率・学習パターン Top20）。
   RBAC は Admin リソース、全操作監査ログ記録。
4. **env 設定**（ハードコーディングなし）: `OPEN_RUNO_HTML_CACHE=on`、
   `_TTL_SECS` `_MAX_ENTRIES` `_MIN_HITS` `_AI=off` `_REFRESH_RATIO`、
   AI 側 `OPEN_RUNO_CACHE_AI_{MIN,MAX,DEFAULT}_TTL_SECS` `_MAX_TRACKED`
   `_MIN_EXPECTED_HITS`
5. **検証**: check 警告ゼロ、全 15 スイート **152 テスト成功**。
6. `.github/workflows/ci.yml` 追加（fmt / clippy -D warnings / test / redis feature）。

### Phase E 実装完了（2026-07-04、164 テスト）— 自己運用・自己修復・二重バックアップ

1. **KeyGuardian（API キー完全自動運用）**（`router::keyring`）:
   人手による鍵管理は不要。SCIM でユーザー登録 → ロール付き鍵を**自動発行**
   （応答の `urn:RCosmo:params:scim:api-key` に一度だけ平文表示、保存は SHA-256）。
   無効化・削除 → **自動失効**。期限切れ → 参照時に**自動掃除**。
   鍵ごとの利用レートを EWMA 学習し、盗難鍵の暴走のような異常は**自動隔離**
   → 冷却後**自動復帰**（env: `OPEN_RUNO_KEY_ANOMALY_FACTOR` 等）。
   台帳が空の間は従来どおり寛容（開発モード）、鍵が 1 本でも発行されると自動で厳格化。
2. **AI 学習モデルの二重永続化**（`router::maintenance`）:
   `ai_model` テーブルは DUAL ルーティング **Both** = PostgreSQL と aruaru-db の
   両方に保存。起動時に自動復元（再起動しても賢いまま）、
   `OPEN_RUNO_AI_PERSIST_SECS`（既定 300 秒）ごとに自動保存。
3. **整合性の AI 自動検証・自動修復**（`DbBackend::consistency_check_and_heal`）:
   Both テーブルを両 DB で突き合わせ、「存在する方が正 → 欠損側に複製」
   「JSON として読める方が正 → 破損側を上書き」「両方有効で不一致 → 主系を正」
   のポリシーで**誤り側を削除・正しい方で上書き**。全件を報告
   （REST 応答 + 監査ログ）。`OPEN_RUNO_INTEGRITY_SECS`（既定 1 時間）の定期実行
   + `POST /api/integrity/check` で随時実行。
4. **引っ越し可能ファイル + 二か所バックアップ**:
   全テーブル（全 DATA + 学習記録）を**単一のポータブル JSON** に書き出し。
   `OPEN_RUNO_BACKUP_DIR`（一次）と `OPEN_RUNO_BACKUP_MIRROR_DIR`（二次）の
   **両方へ同時書き込み**。二次に **Google Drive for Desktop の同期フォルダ**
   （例 `G:\マイドライブ\open-runo-backups`）を指定すれば、Google ログイン済みの
   Drive へ自動でクラウド二重化される。`OPEN_RUNO_BACKUP_SECS` で定期実行、
   `POST /api/backup/export` / `POST /api/backup/import` で随時実行・復元。
5. RBAC: `/api/backup` `/api/integrity` は Admin リソース。全操作監査ログ記録。
6. **検証**: check 警告ゼロ、**164 テスト成功**（KeyGuardian 5 / 整合性 2 /
   永続化・バックアップ 3 / E2E 2 を含む）。

### Phase F 実装完了（2026-07-04、176 テスト）— お引越し・復活・変換・分散統合

1. **移行エンジン**（`open-runo-db::migrate`）: `transfer` / `verify` /
   `transfer_verified`。全対応エンジン（PostgreSQL / MySQL / SQLite /
   aruaru-db / CockroachDB / YugabyteDB / MongoDB / Redis / ClickHouse）は
   同一 `DbBackend` トレイトなので **MySQL→PostgreSQL→CockroachDB など任意の
   組合せを 1 関数で変換**。転送後の自動照合（欠損・不一致検出）付き。
2. **FederatedBackend**（`open-runo-db::federated`）: 社内に散らばる複数 DB を
   1 つの DB として統合運用。テーブル単位ルーティング + broadcast（全拠点複製）
   + 読み取り全メンバーフォールバック。`migrate::transfer_verified` と併用して
   **無停止の段階的片寄せ**が可能。
3. **簡単復活**: `POST /api/backup/restore-latest` — 一次・ミラー
   （Google Drive フォルダ含む）から最新バックアップを自動発見して一発復元。
4. **エンジン変換エクスポート**: `POST /api/migrate/export-sql`
   （dialect: postgres / mysql / generic）と `POST /api/migrate/export-csv`
   （RFC 4180、**Snowflake の COPY INTO 用**）。どちらも二か所へ書き込み。
5. `docs/migration.md` 新設（お引越し 3 手順・復活・変換・統合・分散バックアップ）。
6. RBAC: `/api/migrate` も Admin。全操作監査ログ。
7. **検証**: check 警告ゼロ、**176 テスト成功**（migrate 3 / federated 6 /
   復活・SQL・CSV 3 を含む）。

### FederatedBackend の TOML 設定化（2026-07-11 実装完了）

`open-runo-db::federated_config`（新規モジュール）: `FederatedConfig::from_file`
/ `from_toml_str` で `[[members]] name/kind/url` + `[routes]` +
`broadcast = [...]` + `default_member` を TOML から読み込み、
`FederatedConfig::connect().await` が各メンバーへ実際に接続して
`FederatedBackend` を組み立てる（`kind` は `postgres`/`mysql`/`sqlite`/
`aruaru`/`cockroach`/`yugabyte`/`mongodb`/`redis`/`clickhouse`/`in-memory`
に対応、対応する Cargo feature が無効なら分かりやすいエラー）。
ワークスペース依存に `toml = "0.8"` を追加。テスト 5 件
（parse・接続roundtrip・空メンバー拒否・不明kind拒否・不正TOML拒否・
ファイル未存在）を追加、`open-runo-db` は 27 テスト成功。

ついでに検証中に見つかった既存バグを修正: `clickhouse` feature 有効時に
`ClickHouseBackend`（`clickhouse::Client` を包む）へ `#[derive(Debug)]`
していたが `clickhouse::Client` が `Debug` 未実装でコンパイル不能
だった（`--features full`/`clickhouse` でのみ露見、デフォルト
featureでは無関係だったため今まで見逃されていた）。手動 `Debug` impl に
置き換えて解消、`cargo check -p open-runo-db --features full` が通ることを確認。

なお `mongodb` feature は別の既存バグ（mongodb クレート 3.7 系で
`find_one`/`delete_one`/`find` の API が変わり引数が合わない）で
コンパイル不能なままで未修正（デフォルト feature には影響しないため
今回のスコープ外、別タスクとして切り出し済み）。次回パスが拾える。

### 次セッション候補

- Google Drive API 直接統合（OAuth デバイスフロー、Drive for Desktop 不要化）
- `mongodb` feature のコンパイルエラー修正（mongodb 3.7 API 変更対応、上記参照）
- 個人化部分の分離パターン（共通 HTML + JS Fetch）のサンプル実装
- per-field `@cacheControl`、JWKS 定期リフレッシュ、README 刷新
- `docs/cosmo-parity.md` 4a 節の残りギャップ（EDFS/Kafka連携、gRPC Connect対応、
  Feature Flags、MCP Server統合）

---

## 1. プロジェクト概要

**RCosmo**（旧称: OpenCosmo）は `F:\open-aruaru\RCosmo\` に存在する  
Rust + Poem 製の **GraphQL Federation プラットフォーム / Web フレームワーク**であり、
open-aruaru エコシステムの中心基盤。

> **呼称について（2026-07-04 決定）**: 単体の部品（認証・キャッシュ等）は
> poem の「ミドルウェア」だが、RCosmo 全体は 15 クレートの上に
> サブプロジェクト（open-e-gov / OpenRedmine / OpenWordPress）を構築する
> 基盤なので、対外的には **「フレームワーク」または「プラットフォーム」**
> と表記する（Cosmo も "GraphQL federation platform" を自称）。

WunderGraph Cosmo（Go 製）の問題（低速・GC・型安全性不足）を解決するために、  
Pure Rust で 0 から再実装した次世代 Web ミドルウェア。

### open-aruaru プロジェクト群での位置づけ

```
F:\open-aruaru\
├── RCosmo\          ← 本プロジェクト（中心ミドルウェア）
├── aruaru-db\          ← Pure Rust Git-on-SQL 分散 DB（pgwire :5433）
├── aruaru-ai\          ← AI モデル選択・エージェント基盤
├── aruaru-web\         ← aruaru-web ダッシュボード（Poem + TypeScript）
├── open-cuda\          ← GPU 計算クレート
└── docs\               ← プロジェクト横断ドキュメント
```

RCosmo は以下の全サブプロジェクトのバックボーンとして機能する:
- **open-e-gov** (電子政府)
- **OpenRedmine** (Rust+Poem 版 Redmine)
- **OpenWordPress** (Rust+Poem 版 WordPress 互換)
- **aruaru-llm** (独自 LLM)
- **OpenDirectX / OpenCuda**

---

## 2. 技術スタック

| 層 | 採用技術 |
|----|---------|
| 言語 | Rust (edition 2021, MSRV 1.80) |
| HTTP フレームワーク | Poem 3.x + Tokio |
| デスクトップ | Tauri 2 + TypeScript + Vite |
| GraphQL | async-graphql + Federation |
| データベース | DUAL DATABASE (PostgreSQL :5432 + aruaru-db :5433) |
| キャッシュ | Redis / KeyDB / DragonflyDB :6379 |
| 分析 | ClickHouse :8123 |
| 認証 | X-Api-Key ヘッダ（ApiKeyAuth middleware） |
| 監視 | tracing + opentelemetry（open-runo-observability） |
| ビルド | cargo workspace（11 クレート） |

---

## 3. ワークスペース構造

```
RCosmo/
├── Cargo.toml                   ← workspace root（11 クレート）
├── crates/
│   ├── open-runo-core/           ← 共通型（AppError, Config, Result）
│   ├── open-runo-router/         ← HTTP ゲートウェイ（メイン実行バイナリ）
│   ├── open-runo-db/             ← DUAL DATABASE 抽象化レイヤ
│   ├── open-runo-federation/     ← GraphQL Federation エンジン
│   ├── open-runo-schema-registry/← スキーマ管理
│   ├── open-runo-history/        ← 変更履歴
│   ├── open-runo-ai-routing/     ← AI プロバイダ選択
│   ├── open-runo-security/       ← セキュリティ基盤
│   ├── open-runo-observability/  ← ログ・トレース
│   ├── open-runo-versionless-api/← VersionlessAPI エンジン
│   └── open-runo-backup/         ← バックアップ
├── apps/
│   └── desktop/                 ← Tauri 2 デスクトップアプリ
│       ├── src-tauri/           ← Rust バックエンド（6 Tauri コマンド）
│       ├── src/                 ← TypeScript + Bootstrap 5 フロント
│       └── index.html           ← SPA エントリポイント
└── docs/
    ├── api-spec.md              ← 全エンドポイント仕様
    ├── architecture.md          ← アーキテクチャ図
    ├── database.md              ← DUAL DATABASE 設計
    ├── why-RCosmo.md          ← REST API との比較表
    └── HANDOFF.md               ← 本ファイル
```

---

## 4. 実装済み REST エンドポイント（open-runo-router）

| Method | Path | 説明 |
|--------|------|------|
| GET | `/health` | ヘルスチェック（認証不要） |
| GET | `/healthz` | Kubernetes liveness probe（認証不要） |
| POST | `/api/schemas` | スキーマ登録 |
| GET | `/api/schemas/:service` | スキーマ取得 |
| GET | `/api/schemas/:service/history` | スキーマ履歴 |
| POST | `/api/federation/compose` | Federation 合成 |
| GET | `/api/federation/status` | Federation 状態 |
| POST | `/api/ai/route` | AI プロバイダ選択 |
| GET | `/api/db/status` | DB バックエンド確認 |
| GET | `/api/db/routing` | テーブルルーティング確認 |
| GET | `/api/db/:table` | テーブル一覧 |
| GET | `/api/db/:table/:key` | レコード取得 |
| PUT | `/api/db/:table/:key` | レコード upsert |
| DELETE | `/api/db/:table/:key` | レコード削除 |

全 `/api/*` は `X-Api-Key` ヘッダ必須（`/health`・`/healthz` は免除）。

---

## 5. open-runo-db — サポート DB

`DbBackend` トレイトで全 DB を統一。feature フラグで切り替える。

| feature | Backend | 接続先 |
|---------|---------|--------|
| (常時) | InMemoryBackend | メモリ（テスト用） |
| `postgres` | PostgresBackend | PostgreSQL :5432 |
| `mysql` | MySqlBackend | MySQL 8 / MariaDB :3306 |
| `sqlite` | SqliteBackend | SQLite (file/:memory:) |
| `aruaru` | AruaruDbBackend | aruaru-db :5433 (pgwire) |
| `cockroach` | CockroachBackend | CockroachDB :26257 |
| `yugabyte` | YugabyteBackend | YugabyteDB (pgwire) |
| `mongodb` | MongoBackend | MongoDB :27017 |
| `redis` | RedisBackend | Redis :6379 |
| `clickhouse` | ClickHouseBackend | ClickHouse :8123 |

複合プリセット: `dual`（標準）/ `single-pg` / `single-my` / `dev` / `full` / `cluster`

DualBackend のルーティング（テーブル名で自動振り分け）:
- `sessions` / `api_keys` / `rate_limits` → PostgreSQL のみ
- `schemas` / `backup_jobs` → 両方（耐久性）
- `schema_history` / `change_records` / `audit_log` → aruaru-db のみ

---

## 6. AppState（open-runo-router の共有状態）

```rust
pub struct AppState {
    pub schema_registry: Arc<Mutex<SchemaRegistry>>,
    pub federation_schema: Arc<Mutex<ComposedSchema>>,
    pub history: Arc<Mutex<History>>,
    pub db: Arc<dyn DbBackend>,  // ← DUAL DATABASE（テストは InMemory）
}
```

`AppState::new()` → InMemoryBackend（テスト用）  
`AppState::with_db(arc)` → DualBackend など任意バックエンドを注入

---

## 7. Tauri 2 デスクトップアプリ（apps/desktop/）

`src-tauri/src/lib.rs` に 6 つの Tauri コマンド:
- `health_check` → GET /health
- `register_schema` → POST /api/schemas
- `get_schema` → GET /api/schemas/:service
- `get_schema_history` → GET /api/schemas/:service/history
- `federation_status` → GET /api/federation/status
- `ai_route` → POST /api/ai/route

TypeScript SPA: Bootstrap 5 ダークテーマ、4 ページ（Dashboard / Schemas / Federation / AI Routing）、30 秒ごとヘルスポーリング。

---

## 8. 次にやるべきこと（優先度順）

### 高優先度（次セッションで着手）

1. **WebSocket / SSE エンドポイント追加**  
   `GET /api/events` — SSE でリアルタイム更新をクライアントにプッシュ  
   ファイル: `crates/open-runo-router/src/handlers/events.rs`

2. **CORS ミドルウェア**  
   `crates/open-runo-router/src/middleware/cors.rs`  
   Poem の `CorsMiddleware` を設定する

3. **JWT 認証オプション**  
   現在は X-Api-Key のみ。JWT Bearer token も受け付けられるよう  
   `auth.rs` を拡張する

4. **スキーマバリデーション**  
   リクエストボディの JSON スキーマ検証を handler レベルで追加

5. **open-runo-gateway クレート**  
   GraphQL エンドポイント `POST /graphql` を実装し、  
   Federation された単一 GraphQL インターフェイスを公開する

### 中優先度

6. **RCosmo を中心ミドルウェア化するアーキテクチャ更新**  
   `docs/architecture.md` に RCosmo が open-e-gov / OpenRedmine / OpenWordPress の  
   共通バックボーンとして機能する全体図を追加

7. **DUAL DATABASE のシングル対応を AppState に統合**  
   `DualBackend::single(backend)` ラッパーを追加して  
   シングル DB 環境でも同じコードパスを通る設計にする

8. **Tauri コマンドに DB 操作を追加**  
   `db_get` / `db_put` / `db_list` Tauri コマンドを apps/desktop に追加

### 低優先度（将来）

9. OpenAPI / Swagger UI の自動生成（Poem の OpenAPI 機能活用）
10. GraphQL Subscriptions（WebSocket over `/graphql`）
11. aruaru-db ブランチ操作 API（`/api/db/branches`）
12. ClickHouse 連携ダッシュボード

---

## 9. 重要な設計判断と経緯

| 判断 | 理由 |
|------|------|
| 名称を OpenCosmo → RCosmo に変更 | 商標問題の懸念 |
| aruaru-db を pgwire で接続 | sqlx::PgPool 1 種で PostgreSQL と共通ドライバが使える |
| TypeScript + Tauri を採用（最初は否定後に変更） | ユーザーが仕様変更を決定 |
| VersionlessAPI（/v1 /v2 なし） | バージョン爆発問題の根本解決 |
| feature フラグで DB を切り替え | テスト時は InMemory、本番は Dual/Full |
| X-Api-Key を health 以外に必須 | シンプルな認証。JWT は拡張予定 |

---

## 10. よく使うコマンド

```bash
# ビルド確認
cargo check -p open-runo-router

# テスト（全クレート）
cargo test --workspace

# router のみテスト
cargo test -p open-runo-router

# dual feature で DB クレートをテスト
cargo test -p open-runo-db --features dual

# router 起動（バイナリ）
cargo run -p open-runo-router

# Tauri デスクトップ起動
cd apps/desktop && npm install && npm run tauri dev
```

---

## 11. 関連ドキュメント（RCosmo 内）

- `docs/api-spec.md` — 全エンドポイントの Request/Response 詳細
- `docs/architecture.md` — システム構成図
- `docs/database.md` — DUAL DATABASE ルーティング詳細
- `docs/why-RCosmo.md` — REST API vs Cosmo vs RCosmo 比較表
- `docs/federation.md` — GraphQL Federation 設計
- `docs/versionless-api.md` — VersionlessAPI 仕様
- `docs/security.md` — 認証・レートリミット設計

## 12. 関連ドキュメント（open-aruaru 全体）

- `F:\open-aruaru\docs\database.md` — システム全体 DUAL DATABASE 設計
- `F:\open-aruaru\docs\SPEC_v0.3-second-kusanagi-vision.md` — 製品ビジョン
- `F:\open-aruaru\docs\STACK_SELECTOR.md` — 技術スタック選択ガイド
