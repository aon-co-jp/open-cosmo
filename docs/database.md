# Database Coordination Layer — DUAL DATABASE

Implemented in [`crates/open-runo-db`](../crates/open-runo-db).

---

## なぜ DUAL DATABASE か

| | PostgreSQL | aruaru-db |
|---|---|---|
| **種別** | 汎用 OLTP リレーショナル DB | Pure Rust 製 Git-on-SQL 分散 DB |
| **接続** | `postgres://…:5432` | `postgres://…:5433`（pgwire 互換）|
| **強み** | 高い実績・エコシステム・ACID | データをコミット履歴で管理・時系列クエリ・列指向分析 |
| **弱み** | 変更履歴を自分で実装する必要がある | まだ開発中（v0.5.x）|
| **用途** | セッション・APIキー・OLTP hot-path | スキーマ履歴・監査ログ・分析ワークロード |

aruaru-db は PostgreSQL wire protocol（pgwire）を実装しているため、
`sqlx::PgPool` を使って同じドライバで接続できます。RCosmo の観点からは
「接続先 URL が違うだけで同じインターフェイス」です。

---

## ルーティング戦略

`DualBackend` が論理テーブル名に基づいてバックエンドを選択します。

```
書き込み要求
    │
    ▼
DualBackend::put(table, key, value)
    │
    ├── table == "sessions" / "api_keys" / "rate_limits"
    │       → PostgreSQL のみ（OLTP 最優先）
    │
    ├── table == "schemas" / "backup_jobs"
    │       → PostgreSQL ＋ aruaru-db 両方（耐久性）
    │
    └── table == "schema_history" / "change_records" / "audit_log"
            → aruaru-db のみ（Git-on-SQL で自動バージョン管理）
```

### テーブル→ターゲット対応表

| 論理テーブル | ターゲット | 理由 |
|---|---|---|
| `sessions` | PostgreSQL | 高頻度 OLTP・Git 管理不要 |
| `api_keys` | PostgreSQL | セキュリティ系・参照が多い |
| `rate_limits` | PostgreSQL | 超高頻度・低レイテンシ必須 |
| `schemas` | **両方** | 現行スキーマ + バージョン履歴の両方が必要 |
| `backup_jobs` | **両方** | 耐久性を最大化 |
| `schema_history` | aruaru-db | 全変更を Git コミット形式で保存 |
| `change_records` | aruaru-db | 承認フロー・ロールバック対象 |
| `audit_log` | aruaru-db | 不変監査証跡・時系列クエリ |

---

## クレート構造

```
crates/open-runo-db/src/
├── lib.rs           ← DbBackend trait / InMemoryBackend / PostgresBackend / AruaruDbBackend
├── dual.rs          ← DualBackend / DatabaseTarget / RoutingTable
└── migration.rs     ← CREATE TABLE kv_store (idempotent DDL)
```

### Feature flags

```toml
# Cargo.toml of the consuming crate
open-runo-db = { workspace = true, features = ["dual"] }
# "dual" = "postgres" + "aruaru" (both sqlx::PgPool backends)
```

---

## 起動フロー

```rust
// 1. 接続
let pg     = PostgresBackend::connect(&pg_url).await?;
let aruaru = AruaruDbBackend::connect(&aruaru_url).await?;

// 2. マイグレーション（両 DB に kv_store テーブルを作成）
migration::postgres::run(pg.pool()).await?;
migration::aruaru::run(aruaru.pool()).await?;

// 3. DualBackend を組み立て
let db = DualBackend::with_default_routing(Arc::new(pg), Arc::new(aruaru));

// 4. AppState に渡す
let state = AppState { db: Arc::new(db), .. };
```

---

## aruaru-db について

aruaru-db は RCosmo と同じ `open-aruaru` プロジェクト内で開発している
Pure Rust 製の分散データベースです。

主な特徴：

- **Git-on-SQL** — Prolly Tree でデータをバージョン管理。`git commit / branch / merge` 相当の操作が SQL でできる
- **PostgreSQL wire 互換** — psql / DBeaver / sqlx がそのまま接続できる（ポート 5433）
- **列指向分析** — Apache Arrow + DataFusion による高速 OLAP クエリ
- **Raft 分散合意** — `openraft` による強整合クラスタ
- **Versionless GraphQL** — `async-graphql` によるスキーマ進化対応 GraphQL API

詳細は [`../../aruaru-db/ARCHITECTURE.md`](../../aruaru-db/ARCHITECTURE.md) を参照してください。

---

## 今後の拡張予定

- `DualBackend::list_with_history(table, key)` — aruaru-db のコミット履歴を取得する API
- 時系列クエリ: `AS OF COMMIT '<hash>'` のラッパー
- `DualBackend` への `open-runo-router` エンドポイント追加（`/api/db/history`, `/api/db/branches`）
