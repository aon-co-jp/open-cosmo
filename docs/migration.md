# 簡単お引越し・簡単復活ガイド

open-cosmo の全データ（12 テーブル + AI 学習記録）は `DbBackend` 抽象の上に
あるため、**どのデータベース間でも同じ手順で引っ越し**できます。

---

## 1. 簡単お引越し（サーバー移転）

旧サーバーで書き出し → ファイルを持って行く → 新サーバーで取り込み。3 手で完了。

```bash
# 旧サーバー: ポータブルファイルを書き出し（一次 + ミラーの二か所へ）
curl -X POST -H "x-api-key: $KEY" http://old:8080/api/backup/export

# ファイルを新サーバーへコピー（USB でも scp でも Google Drive 経由でも）

# 新サーバー: 取り込み
curl -X POST -H "x-api-key: $KEY" \
     -d '{"path":"/backups/open-runo-backup-20260704-120000.json"}' \
     http://new:8080/api/backup/import
```

## 2. 簡単復活（ワンコール・リストア）

ファイル名を覚えていなくても、一次・ミラー（Google Drive フォルダ含む）を
探索して**最新のバックアップから一発復元**します。

```bash
curl -X POST -H "x-api-key: $KEY" http://host:8080/api/backup/restore-latest
```

## 3. データベースエンジンの変換

### 3.1 open-cosmo 対応エンジン間（MySQL → PostgreSQL → CockroachDB 等）

対応エンジン（PostgreSQL / MySQL / SQLite / aruaru-db / CockroachDB /
YugabyteDB / MongoDB / Redis / ClickHouse）はすべて同じ `DbBackend` なので、
コードからは 1 関数で変換完了（転送後の自動照合付き）:

```rust
use open_runo_db::migrate;

let (report, issues) =
    migrate::transfer_verified(&*mysql, &*postgres, ALL_TABLES).await?;
assert!(issues.is_empty()); // 完全一致を検証してから切り替え
```

### 3.2 Snowflake などトレイト外エンジンへ

変換ファイルを生成します（どちらも一次 + ミラーの二か所へ書き込み）:

```bash
# SQL ダンプ（dialect: postgres = CockroachDB/YugabyteDB 兼用 / mysql / generic）
curl -X POST -H "x-api-key: $KEY" -d '{"dialect":"generic"}' \
     http://host:8080/api/migrate/export-sql

# CSV（Snowflake はこちら推奨）
curl -X POST -H "x-api-key: $KEY" http://host:8080/api/migrate/export-csv
```

Snowflake 側の取り込み:

```sql
CREATE TABLE open_runo_kv (tbl STRING, k STRING, v STRING);
COPY INTO open_runo_kv FROM @your_stage/open-runo-dump-....csv
  FILE_FORMAT = (TYPE = CSV SKIP_HEADER = 1 FIELD_OPTIONALLY_ENCLOSED_BY = '"');
```

## 4. 社内分散データベースの統合運用（FederatedBackend）

拠点ごとに散らばった DB を **1 つのデータベースとして**運用しながら、
段階的に片寄せできます（ビッグバン移行不要）:

```rust
use open_runo_db::federated::FederatedBackend;

let fed = FederatedBackend::builder()
    .member("tokyo-pg",  tokyo_postgres)   // 本社 PostgreSQL
    .member("osaka-my",  osaka_mysql)      // 支社 MySQL
    .member("archive",   clickhouse)       // 分析基盤
    .route("orders",    "osaka-my")        // 所有チームの DB に残す
    .route("audit_log", "archive")
    .broadcast("schemas")                  // 重要データは全拠点に複製
    .default_member("tokyo-pg")
    .build()?;

let state = AppState::with_db(Arc::new(fed)); // 以降は 1 つの DB として運用
```

- 読み取りは全メンバーへフォールバック: どこにあっても見つかる
- 統合は `migrate::transfer_verified(osaka, tokyo, &["orders"])` を
  テーブル単位で実行 → 検証 → `route` を書き換えるだけ。無停止で完了

## 5. 定期分散バックアップ（二か所以上）

```bash
OPEN_RUNO_BACKUP_DIR=D:\open-runo-backups          # 一次（ローカル）
OPEN_RUNO_BACKUP_MIRROR_DIR=G:\マイドライブ\orn     # 二次（Google Drive 同期）
OPEN_RUNO_BACKUP_SECS=3600                          # 1 時間ごと自動
OPEN_RUNO_INTEGRITY_SECS=3600                       # 両 DB 整合性の自動検証・自動修復
OPEN_RUNO_AI_PERSIST_SECS=300                       # AI 学習の自動保存（両 DB）
```

さらに増やしたい場合は、ミラー先を Dropbox/OneDrive の同期フォルダにした
インスタンスを併走させるか、`/api/backup/export` を外部スケジューラから
複数回呼び出してください。
