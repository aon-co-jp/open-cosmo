# Cosmo 互換ロードマップ — open-runo を GraphQL ファーストへ

> **仕様変更日**: 2026-07-03
> **方針**: WunderGraph Cosmo（Apache 2.0 / Go 製）の無償版 + **有料版（Launch / Scale / Enterprise）機能**を
> Pure Rust + Poem で互換実装し、GitHub 上のオープンソース・フレームワーク
> （GraphQL Federation プラットフォーム）として提供する。
> 自前サーバー / AWS / VPS / クラウドいずれでも同一バイナリで動作すること。

参考: <https://wundergraph.com/cosmo> / <https://cosmo-docs.wundergraph.com/overview> / <https://graphql.org/>

---

## 1. 基本方針の変更点

| 項目 | 旧方針 | 新方針 |
|------|--------|--------|
| API の主軸 | REST (VersionlessAPI) が主、GraphQL は追加 | **GraphQL (Federation) が主軸**。REST は管理・互換 API として維持 |
| 互換ターゲット | Cosmo OSS 相当 | **Cosmo 有料版（Launch/Scale/Enterprise）機能まで互換** |
| 提供形態 | 未定 | GitHub で全機能 OSS 公開（機能制限なし・Apache 2.0 OR MIT） |
| 収益版との差 | — | open-runo では**全機能を無償開放**（リクエスト数制限・チーム人数制限・データ保持制限を設けない） |

Cosmo が有料プランでのみ解放している機能を、open-runo はすべて OSS で提供する。
これが open-runo の最大の差別化ポイントとなる。

---

## 2. Cosmo 機能 → open-runo クレート対応表

### 2.1 無償版 (Developer/OSS) 相当 — 既存実装

| Cosmo 機能 | open-runo 対応 | 状態 |
|-----------|----------------|------|
| Cosmo Router（クエリエンジン） | `open-runo-router` + `open-runo-gateway` (POST /graphql) | ✅ 実装済み |
| Schema Registry | `open-runo-schema-registry` (+ REST /api/schemas) | ✅ 実装済み |
| Federation 合成・破壊的変更検出 | `open-runo-federation` | ✅ 実装済み（SDL 完全対応は今後） |
| Cosmo Studio（ダッシュボード） | `apps/desktop-wasm` (Rust→WebAssembly) → 将来 aruaru-web 版 | ✅ 基本 5 ページ |
| OTEL テレメトリ | `open-runo-observability` | 🔶 tracing のみ。OTLP export 拡張予定 |

### 2.2 有料版 (Launch / Scale) 相当 — 次の実装ターゲット

| Cosmo 有料機能 | open-runo 実装先 | 優先度 | 内容 |
|---------------|-----------------|--------|------|
| **Persisted Queries / Trusted Documents** | `open-runo-persisted-queries` + gateway 統合 | ✅ 実装済み | SHA-256 登録・APQ 互換。`/graphql` が `OPEN_RUNO_PQ_MODE=disabled\|allow\|enforce` で動作。REST: `/api/persisted-queries` |
| **細粒度レートリミット** | `open-runo-security::TokenBucketLimiter` | ✅ 実装済み | per-key トークンバケット + `with_override`。ルート単位のミドルウェア化は今後 |
| **キャッシュ制御 (Response / Entity Cache)** | `open-runo-cache` + gateway 統合 | ✅ 実装済み | `OPEN_RUNO_CACHE=on` で operation 単位の TTL レスポンスキャッシュ。`redis-backend` feature で Redis 共有。per-field `@cacheControl` は今後 |
| **月間リクエスト数の計測**（制限はしない） | `open-runo-observability` + ClickHouse | ★★☆ | 課金目的ではなく運用メトリクスとして計測のみ |
| **Analytics / Tracing (Studio 相当)** | ClickHouse :8123 + aruaru-web ダッシュボード | ★★☆ | オペレーション別レイテンシ・エラー率 |

### 2.3 Enterprise 相当 — 中期実装ターゲット

| Cosmo Enterprise 機能 | open-runo 実装先 | 優先度 | 内容 |
|----------------------|-----------------|--------|------|
| **SSO (OIDC)** | `open-runo-security::oidc` + `ApiKeyAuth::with_oidc` | ✅ 実装済み | JWKS/RS256 検証（kid・iss・aud・exp）。env: `OPEN_RUNO_OIDC_ISSUER` / `OPEN_RUNO_OIDC_JWKS_FILE`。Discovery 自動フェッチは今後 |
| **厳密な RBAC** | `open-runo-security::rbac` + `ApiKeyAuth::with_rbac` | ✅ 実装済み | `OPEN_RUNO_RBAC=enforce` で JWT roles をルート単位に認可（403）。admin/developer/viewer 組み込み |
| **SCIM (ユーザープロビジョニング)** | `open-runo-scim` + `/scim/v2/Users` `/scim/v2/Groups` | ✅ 実装済み | RFC 7643/7644 Users + Groups CRUD、`OPEN_RUNO_SCIM_TOKEN`（IdP 用 Bearer） |
| **監査ログ** | `open-runo-router::audit` + `audit_log` テーブル | ✅ 実装済み | schema / db / persisted-query / SCIM の全変更を actor 付きで aruaru-db (Git-on-SQL) に記録 |
| **マルチグラフ / namespace** | `SchemaRegistry::register_in` ほか + REST `?namespace=` | ✅ 実装済み | namespace ごとに独立したグラフ。既存 API は `default` namespace で完全互換 |
| **SOC2/HIPAA 対応基盤** | docs/security.md 拡充 | ★☆☆ | 暗号化・保持ポリシー・アクセス制御の文書化（認証取得は範囲外） |
| GraphQL Subscriptions (WS/SSE) | `SubscriptionRoot::schema_events` + `GET /graphql/ws` | ✅ 実装済み | graphql-ws プロトコル。broadcast ブローカーでスキーマ変更を配信 |

---

## 3. 実装フェーズ

1. **Phase A（次セッション）**: `open-runo-persisted-queries` クレート新設、
   `open-runo-security` のレートリミット拡張（per-key トークンバケット）、RBAC マトリクス設計。
2. **Phase B**: OIDC SSO（JWKS 検証）、監査ログ配線、`@cacheControl` + Redis キャッシュ。
3. **Phase C**: SCIM 2.0、マルチグラフ/namespace、GraphQL Subscriptions、
   ClickHouse アナリティクス + aruaru-web ダッシュボード統合。

すべて feature フラグで無効化可能にし、最小構成（シングルバイナリ + SQLite +
`DualBackend::single`）でも VPS 1 台で動くことを維持する。

---

## 4a. 未実装・要検討ギャップ（2026-07-11、公式ドキュメント調査）

<https://wundergraph.com/cosmo> と <https://cosmo-docs.wundergraph.com/enterprise>
を実際にWeb調査し、上記の対応表と照合した結果、open-runoに**まだ存在しない**
Cosmoの機能を洗い出した。優先度は「REST APIを不要にする」という本プロジェクトの
目的への寄与度で判断。

| Cosmo機能 | 内容 | open-runoの状況 | 優先度 |
|-----------|------|-----------------|--------|
| **Event-Driven Federated Subscriptions (EDFS) / Cosmo Streams** | Kafka/NATS/Redis をGraphQL Subscriptionsのイベント源として統合 | 未実装(現状はin-processブロードキャストのみ) | ★★☆ 分散環境では必要 |
| **gRPC対応 (Cosmo Connect)** | 既存gRPCサービスをGraphQLスキーマとして federation に取り込む | 未実装 | ★★☆ 社内マイクロサービス統合に有用 |
| ~~**Feature Flags(スキーマ変更・トラフィックルーティング)**~~ | カナリアリリース、プレビュー環境へのトラフィック振り分け | **2026-07-11実装済み**(open-runo側で先行実装、こちらへ逆方向ミラー): 新規crate`crates/open-runo-feature-flags`(`FeatureFlagRegistry`: `upsert`/`get`/`list`/`delete`/`evaluate(name, bucket_key) -> Option<bool>`、`DefaultHasher`による決定的0-100バケッティングでrollout_percent%のトラフィックを同一callerに対し常に同じ側へ振り分け)。REST: `POST /api/feature-flags`(upsert)・`GET /api/feature-flags`(list)・`GET /api/feature-flags/:name`(get、404)・`DELETE /api/feature-flags/:name`(delete、404)・`GET /api/feature-flags/:name/evaluate?bucket_key=...`(evaluate、flag自体が未知なら404)。`open-runo-api-types`に`FeatureFlagRequest`/`FeatureFlagResponse`/`FeatureFlagListResponse`/`FeatureFlagEvaluationResponse`追加。実バイナリ+curlでself-issue→upsert→get→list→evaluate→evaluate-unknown(404)→no-key(401)→delete→get(404)のroundtripを確認済み | ✅ 完了 |
| ~~**Powerful CLI (`wgc`相当)**~~ | スキーマ検索・ドキュメント生成・composition checkをCLIから実行 | **2026-07-11実装済み**: 新規crate `crates/open-runo-cli`(バイナリ`open-runo-cli`)。`schema register/get/history`・`federation status`・`openapi`(OpenAPI 3.0スペックのdump=ドキュメント生成相当)・`login`を実装。`--api-key`未指定時は`POST /api/keys/self-issue`で自動取得(人間がAPIキーを意識しないという既存方針を踏襲)。`--json`で生JSON、デフォルトは人間可読出力。実バイナリ(`cargo run -p open-runo-router`)に対しCLIから登録→取得→履歴確認のroundtripおよび404エラーパスを実際に確認済み | ✅ 完了 |
| ~~**MCP (Model Context Protocol) Server統合**~~ | LLMエージェントがCosmoのスキーマ/データを直接操作できるMCPサーバー | **2026-07-12実装済み**(poem-cosmo-tauriからミラー済み): `crates/open-runo-router/src/mcp.rs`(`POST /mcp`、新規依存無し)。JSON-RPC 2.0 Streamable HTTP transportで`initialize`/`tools/list`/`tools/call`/`resources/list`/`resources/read`/`prompts/list`/`prompts/get`を実装済み。本節の記述が古いまま未反映で残っていたため今回同期(コードの追加変更なし、ドキュメント整合性のみの修正) | ✅ 完了 |
| ~~**Federation v1 互換の明示**~~ | v1/v2両対応を明言 | **2026-07-11実装済み**: 調査の結果、既存の`compose()`はSDLを一切パースせず(呼び出し側が事前に`{service_name, types: {Type: [fields]}}`というJSONへ手作業で変換する必要があった)、そもそもv1/v2という区別が生じる余地がなかった(真のギャップは「SDLベースの合成が一切存在しない」ことだった)。新規`crates/open-runo-federation/src/sdl.rs`に手書きSDLパーサー`parse_service_sdl(service_name, sdl) -> Result<ServiceSchema>`(`type`/`interface`/`extend type`ブロックからフィールド名を抽出、ディレクティブ・引数・`implements`句は両方言で同じ構文位置に現れるため汎用的にスキップ)と`detect_federation_version(sdl) -> FederationVersion`(`@link(url: "https://specs.apollo.dev/federation/v2...")`があればV2、無くても`@key`/`@requires`/`@provides`/`@external`があればV1=旧来の暗黙ディレクティブ方式、どちらも無ければNone)を追加。`POST /api/federation/compose`の`ServiceInput`に`sdl: Option<String>`を追加(後方互換、既存の`types`直接指定も引き続き動作)。実バイナリ(`cargo run -p open-runo-router`)に対し、本物のv1スタイル部分グラフ(bare `@key`/`@external`、`@link`無し)と本物のv2スタイル部分グラフ(`@link(url:"https://specs.apollo.dev/federation/v2.3"...)`+`@shareable`)を**同一リクエストで同時に**`POST /api/federation/compose`へ送信し、`User`型のフィールドが両方(v1由来の`name`/`reviews`、v2由来の`plan`/`balanceCents`、共通の`id`)から正しくマージされた1つのスーパーグラフになることを実HTTPで確認済み。ユニットテスト7本追加(v1検出・v2検出・None検出・v1パース・v2パース・v1+v2混在合成・空SDL拒否) | ✅ 完了 |
| ~~**本格的なDistributed Tracing (OTEL export)**~~ | トレースの外部エクスポート(Jaeger/Tempo等) | **2026-07-11実装済み**: `open-runo-observability::init_tracing_with_otlp`が`OPEN_RUNO_OTLP_ENDPOINT`環境変数(未設定時は従来通りコンソールのみ)経由でOTLP HTTPエクスポータ(`opentelemetry-otlp`、非同期reqwestクライアント)を配線。ワークスペースが元々宣言のみで未使用だった`opentelemetry`/`opentelemetry-jaeger`(0.22)を実際に使う最新版(`opentelemetry`/`opentelemetry_sdk`/`opentelemetry-otlp` 0.32、`tracing-opentelemetry` 0.33)に更新し置き換え | ✅ 完了 |

**REST APIを不要にするという核心仕様(GraphQL Federation + VersionlessAPIで
REST APIの乱立を根本解決する)自体は、Cosmo有料版と同一方針で既に実装済み**
(Schema Registry・Federation合成・Persisted Queries・RBAC・OIDC・SCIM・
監査ログ・マルチグラフ・レートリミット・レスポンスキャッシュ・
Subscriptions、いずれも2.1〜2.3節の通り実装済み)。上表のギャップは
「Cosmoにあってopen-runoにまだない付加機能」であり、コア仕様の欠落ではない。

---

## 4. デプロイ形態（すべて同一バイナリ）

| 形態 | 構成 |
|------|------|
| 自前サーバー / VPS (ConoHa 等) | `open-runo-gateway` バイナリ + SQLite or PostgreSQL、systemd |
| AWS / クラウド | 同バイナリ + RDS/ElastiCache、Docker (`Dockerfile` 済) |
| フルスタック | DUAL DATABASE (PostgreSQL + aruaru-db) + Redis + ClickHouse (`--features full`) |

Cosmo と異なり「マネージド版でしか使えない機能」を作らない。
