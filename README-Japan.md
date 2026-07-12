# open-runo

**Rust 製 GraphQL Federation プラットフォーム(Poem/Tauri/Cosmoは非依存・互換自前実装)**
— WunderGraph Cosmo の有料版機能を OSS で。独自の自己学習 AI 搭載(外部 LLM 契約不要)。
[poem-cosmo-tauri](https://github.com/aon-co-jp/poem-cosmo-tauri)(姉妹リポジトリ)
と同時並行で開発しています(どちらが先行してもよく、乖離に気づいた側がミラー)。
**Poemとブラウザ内実行機能搭載も含めたTauri両方共に、一から開発して完全互換で
再現する。**

[![CI](https://github.com/aon-co-jp/open-runo/actions/workflows/ci.yml/badge.svg)](https://github.com/aon-co-jp/open-runo/actions/workflows/ci.yml)
![Rust](https://img.shields.io/badge/rust-stable-orange)
![License](https://img.shields.io/badge/license-Apache--2.0%20OR%20MIT-blue)
![Tests](https://img.shields.io/badge/tests-316%20passed-brightgreen)

📖 詳細: [日本語 README](README-Japan.md) / [English README](README-English.md) /
[中文](README-Chinese.md) / [한국어](README-Korea.md) / [Español](README-Spain.md) /
[Français](README-France.md) / [Deutsch](README-Germany.md) / [Italiano](README-Italy.md) /
[Русский](README-Russia.md) / [العربية](README-Arabic.md) —
他プロジェクトへの導入は **[PORTING.md](PORTING.md)** 1 枚で完結します。

---

## open-runo とは

REST API の乱立（BFF 地獄・`/v1 /v2` のバージョン爆発・エンドポイント管理の崩壊）を
**GraphQL Federation + VersionlessAPI** で根本解決するプラットフォームです。
Go 製の WunderGraph Cosmo が有料プラン（Launch / Scale / Enterprise）でのみ
提供する機能を、Pure Rust で**すべて無料の OSS として**実装しています。

```
   open-e-gov     OpenRedmine     OpenWordPress     aruaru-llm
       │               │               │                │
       └───────GraphQL (POST /graphql) + REST───────────┘
                           │
                 ┌───────────────────┐        PostgreSQL :5432
                 │     open-runo     │──DUAL──┤
                 │  (このリポジトリ)  │        aruaru-db  :5433
                 └───────────────────┘        Redis / ClickHouse
```

## 機能マトリクス

| 機能 | Cosmo 無料版 | Cosmo 有料版 | **open-runo** |
|------|:---:|:---:|:---:|
| GraphQL Federation / Schema Registry | ✅ | ✅ | ✅ |
| GraphQL Subscriptions (WebSocket) | ✅ | ✅ | ✅ |
| Persisted Queries / Trusted Documents | — | ✅ | ✅ **無料** |
| 厳密な RBAC（ルート単位） | — | ✅ | ✅ **無料** |
| SSO（OIDC / JWKS RS256） | — | ✅ | ✅ **無料** |
| SCIM 2.0 プロビジョニング（Users/Groups） | — | ✅ | ✅ **無料** |
| 監査ログ（Git-on-SQL 保存） | — | ✅ | ✅ **無料** |
| 細粒度レートリミット（トークンバケット） | — | ✅ | ✅ **無料** |
| レスポンスキャッシュ | — | ✅ | ✅ **無料** |
| マルチグラフ / namespace | — | ✅ | ✅ **無料** |
| リクエスト数・チーム人数・保持期間の制限 | あり | 緩和 | **一切なし** |

### open-runo だけの機能

- 🧠 **自己学習 AI**（外部 LLM・有料契約ゼロ）— HTML ページキャッシュの
  自動判定（URL パターン汎化によるコールドスタート予測）、レンダリング
  コスト学習、適応 TTL、先回り再生成（ユーザーが MISS を見ない）
- 🔑 **KeyGuardian** — API キーの完全自動運用: SCIM 連動の自動発行/失効、
  利用パターン学習による盗難鍵の自動隔離→自動復帰
- 🗄️ **DUAL DATABASE** — PostgreSQL + aruaru-db の二重化、整合性の
  自動検証・自動修復（欠損/破損を検知し正しい側で上書き）
- 📦 **簡単お引越し・簡単復活** — 全 DATA + AI 学習を単一ポータブル JSON へ、
  二か所（ローカル + Google Drive 同期フォルダ）に自動バックアップ、
  `restore-latest` ワンコール復元
- 🔀 **エンジン変換・分散統合** — MySQL→PostgreSQL→CockroachDB を 1 関数で
  変換（自動照合つき）、Snowflake 向け SQL/CSV エクスポート、
  FederatedBackend で社内分散 DB を 1 つに統合運用(TOML 1 枚で
  members/routes/broadcast を宣言する設定ファイル読み込みにも対応)
- ⚡ **VersionlessAPI** — `/v1 /v2` を作らない互換性ルールエンジン
- 🖥️ **デスクトップ管理アプリ**(Rust→WebAssembly、Tauri/Node.js/TypeScript不使用)
- ⌨️ **CLI(`open-runo-cli`)** — wgc 相当のスキーマ登録/取得/履歴確認・
  federation status・OpenAPI スペック取得を CLI から実行、APIキーは
  未指定なら自動 self-issue

## クイックスタート

```bash
git clone https://github.com/aon-co-jp/open-runo
cd open-runo
cargo test --workspace          # 307 テスト(--all-features で316)
cargo run -p open-runo-gateway  # REST + GraphQL 統合サーバー起動(poem-free)
```

```bash
# GraphQL（GraphiQL は GET /graphql）
curl -X POST http://localhost:8080/graphql \
     -H 'content-type: application/json' \
     -d '{"query":"{ health }"}'

# スキーマ登録（REST 管理面）
curl -X POST http://localhost:8080/api/schemas \
     -H 'x-api-key: dev-key' \
     -d '{"service_name":"users","sdl":"type User { id: ID! }"}'
```

### 管理UI(WASM フロントエンド)を使う

`cargo run`だけではAPIサーバーが起動するだけで、`GET /`で配信される
管理UI(`apps/desktop-wasm`)本体は初回のみ別途ビルドが必要です:

```bash
rustup target add wasm32-unknown-unknown         # 初回のみ
cargo install wasm-bindgen-cli --version 0.2.126  # 初回のみ(Cargo.lockのバージョンと一致させること)
make wasm-frontend                                # apps/desktop-wasm/www/pkg を生成
cargo run -p open-runo-gateway                    # ビルド済みUIも同じポートで配信
```

`http://localhost:8080/` を開くとDashboard/Schema Registry/Federation/
AI Routing/DUAL DATABASE/SCIM/Persisted Queries/Feature Flags/
Cache & Backup/Analytics(月間リクエスト数・オペレーション別レイテンシ/
エラー率、`docs/cosmo-parity.md` 4a)の10ページ管理UIが使えます。

AI HTML キャッシュを有効化して自分のアプリに載せる例・全環境変数・
全エンドポイントは **[PORTING.md](PORTING.md)** を参照してください。

## ワークスペース構成（17 クレート）

| クレート | 役割 |
|----------|------|
| `open-runo-core` | 共通型（AppError / Config / Result） |
| `open-runo-router` | REST ゲートウェイ・認証(KeyGuardian/RBAC/OIDC/SCIM)・監査・AI HTML キャッシュ・自己保守 |
| `open-runo-gateway` | GraphQL エンドポイント（Federation / Subscriptions / PQ / レスポンスキャッシュ） |
| `open-runo-federation` | スキーマ合成・破壊的変更検出 |
| `open-runo-schema-registry` | バージョン管理・namespace（マルチグラフ） |
| `open-runo-db` | DbBackend 抽象（9 エンジン）・DUAL・Federated（TOML設定対応）・migrate |
| `open-runo-cache` | TTL キャッシュ + 自己学習予測器（Redis backend は feature） |
| `open-runo-security` | API キー・JWT・OIDC・RBAC・レートリミット |
| `open-runo-persisted-queries` | Trusted Documents（SHA-256 / APQ 互換） |
| `open-runo-scim` | SCIM 2.0 Users / Groups |
| `open-runo-ai-routing` | AI プロバイダ選択（コスト/レイテンシ/ローカル/プライバシー） |
| `open-runo-versionless-api` | 互換性ルールエンジン |
| `open-runo-cli` | wgc 相当の CLI（schema register/get/history・federation status・openapi・login） |
| `open-runo-api-types` | REST/CLI 共有の型定義 |
| `open-runo-history` / `-backup` / `-observability` | 変更履歴 / バックアップ / 監視(OTLP export 対応) |

## デプロイ

同一バイナリが自前サーバー / VPS / AWS / Docker すべてで動きます。
最小構成（SQLite 1 台）から `--features full`（DUAL + Redis + ClickHouse）まで
feature フラグで選択。「マネージド版でしか使えない機能」はありません。

## ドキュメント

- [docs/architecture.md](docs/architecture.md) — 全体設計
- [docs/cosmo-parity.md](docs/cosmo-parity.md) — Cosmo 機能対応表
- [docs/poem-parity.md](docs/poem-parity.md) — Poem 機能対応表
- [docs/tauri-parity.md](docs/tauri-parity.md) — Tauri 機能対応表
- [docs/migration.md](docs/migration.md) — お引越し/変換/統合
- [docs/api-spec.md](docs/api-spec.md) — API 仕様
- [docs/security.md](docs/security.md) — セキュリティ
- [docs/HANDOFF.md](docs/HANDOFF.md) — 開発履歴

## 関連プロジェクト

`open-web-server` を中心に、このリポジトリ・`poem-cosmo-tauri`・
PostgreSQL・`aruaru-db`・`open-raid-z` を組み合わせ、3Dオンラインゲームの
課金アイテム・金融/証券データをネットワーク上で紛失させないための
目標アーキテクチャ(通信層四重化・DB書き込み四重化、2026-07-11改訂)が
ある。open-runo は Federation Gateway/バックエンド側として関与しうる
(詳細は [open-web-server](https://github.com/aon-co-jp/open-web-server) の
`README.md`/`CLAUDE.md` を参照)。

## License

Apache-2.0 OR MIT（お好きな方でどうぞ）。
Contribution は [CONTRIBUTING.md](CONTRIBUTING.md) を参照してください。
