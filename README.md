# open-cosmo

**Rust 製 GraphQL Federation プラットフォーム。WunderGraph Cosmo の有料版を含む
全ての機能の完全無料のオープンソース版。WEBサーバー負荷分散やDBバックアップの
分散統合機能なども含みます**(Cosmo自体は着想元のみで実装非依存)。独自の
自己学習 AI 搭載(外部 LLM 契約不要)。
**Tauriや、Poem機能はありません**——それらの機能を実装するのは姉妹リポジトリ
[RPoem](https://github.com/aon-co-jp/RPoem)(旧poem-cosmo-tauri)。

> RPoemと同時並行で開発しています。どちらが先行してもよく、乖離に気づいた側が
> もう一方へミラーする運用です(詳細は共有の `docs/HYBRID_NETWORK_ARCHITECTURE.md`
> §0.5)。共通コア(WunderGraph Cosmoの有料版機能+WEB高速化)はパッケージとして
> 直接依存せず、Rust 標準ライブラリ + tokio/hyper で自前実装しています。
> **このリポジトリ(open-cosmo)にはTauri・Poemの再現実装は含まれません**——それらを
> 含む版はRPoemを参照してください。

[![CI](https://github.com/aon-co-jp/open-cosmo/actions/workflows/ci.yml/badge.svg)](https://github.com/aon-co-jp/open-cosmo/actions/workflows/ci.yml)
![Rust](https://img.shields.io/badge/rust-stable-orange)
![License](https://img.shields.io/badge/license-Apache--2.0%20OR%20MIT-blue)
![Tests](https://img.shields.io/badge/tests-362%20passed-brightgreen)

📖 詳細: [日本語 README](README-Japan.md) / [English README](README-English.md) /
[中文](README-Chinese.md) / [한국어](README-Korea.md) / [Español](README-Spain.md) /
[Français](README-France.md) / [Deutsch](README-Germany.md) / [Italiano](README-Italy.md) /
[Русский](README-Russia.md) / [العربية](README-Arabic.md) —
他プロジェクトへの導入は **[PORTING.md](PORTING.md)** 1 枚で完結します。

---

## open-cosmo とは

REST API の乱立(BFF 地獄・`/v1 /v2` のバージョン爆発・エンドポイント管理の崩壊)を
**GraphQL Federation + VersionlessAPI** で根本解決するプラットフォームです。
Go 製の WunderGraph Cosmo が有料プラン(Launch / Scale / Enterprise)でのみ
提供する機能を、Pure Rust で**すべて無料の OSS として**実装しています。
Tauri・Poem・WunderGraph Cosmo はいずれもパッケージとして直接依存させず、
それぞれの機能・API 形状には互換性を保ちながら Rust 標準ライブラリ +
tokio/hyper で自前実装しています。

**RPoem/open-cosmo は、REST API を不要にするための WunderGraph Cosmo 互換です**(アプリの API は GraphQL に一本化します。管理用の HTTP エンドポイントはスキーマ登録などの運用のために残っています)。

```
   open-e-gov     OpenRedmine     OpenWordPress     aruaru-llm
       │               │               │                │
       └───────GraphQL (POST /graphql)      ───────────┘
                           │
                 ┌───────────────────┐        PostgreSQL :5432
                 │   open-cosmo   │──DUAL──┤
                 │  (このリポジトリ)  │        aruaru-db  :5433
                 └───────────────────┘        Redis / ClickHouse
```

## 機能マトリクス

| 機能 | Cosmo 無料版 | Cosmo 有料版 | **open-cosmo** |
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

### open-cosmo だけの機能

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
- 📎 **Multipart ファイルアップロード** — 手書き RFC 7578 パーサー、
  `POST /api/schemas/upload` でSDLファイルを直接アップロード
- 🍪 **Cookie/セッション + CSRF** — `X-Api-Key` に追加する認証経路、
  `POST /api/session/login`/`logout`、CSRF二重送信トークン検証
- 🔒 **TLS終端**(`tls` feature、rustls) — リバースプロキシ不要で
  直接HTTPS配信可能
- 🖥️ **デスクトップ管理アプリ**(Tauri非依存・互換UI、Rust→WebAssembly、
  Node.js/TypeScript ビルドチェーン不使用)
- 🔔 **システムトレイ + ネイティブ通知 + ネイティブインストーラー**
  (`apps/desktop-tray`、tauriパッケージ非依存。`tray-icon`+`tao`+
  `notify-rust`、Windows向け実.exeインストーラー付き)
- ⌨️ **CLI(`open-runo-cli`)** — wgc 相当のスキーマ登録/取得/履歴確認・
  federation status・OpenAPI スペック取得を CLI から実行、APIキーは
  未指定なら自動 self-issue

## クイックスタート

```bash
git clone https://github.com/aon-co-jp/open-cosmo
cd open-cosmo
cargo test --workspace          # 343 テスト(--all-features で362)
cargo run -p open-runo-gateway  # GraphQL 統合サーバー起動(poem-free、REST API 不要)
```

```bash
# GraphQL（GraphiQL は GET /graphql）
curl -X POST http://localhost:8080/graphql \
     -H 'content-type: application/json' \
     -d '{"query":"{ health }"}'

# スキーマ登録（管理用エンドポイント。アプリの API ではありません）
curl -X POST http://localhost:8080/api/schemas \
     -H 'x-api-key: dev-key' \
     -d '{"service_name":"users","sdl":"type User { id: ID! }"}'
```

### 管理UI(WASM フロントエンド)を使う

`cargo run`だけでは`open-runo-router`/`open-runo-gateway`がAPIサーバーとして
起動しますが、`GET /`で配信される管理UI(`apps/desktop-wasm`)本体は
別途ビルドが必要です(初回・コード変更時のみ):

```bash
rustup target add wasm32-unknown-unknown        # 初回のみ
cargo install wasm-bindgen-cli --version 0.2.126 # 初回のみ(Cargo.lockのバージョンと一致させること)
make wasm-frontend                              # apps/desktop-wasm/www/pkg を生成
cargo run -p open-runo-gateway                  # ビルド済みUIも同じポートで配信される
```

ブラウザで `http://localhost:8080/` を開くと、Dashboard / Schema Registry /
Federation / AI Routing / DUAL DATABASE / SCIM / Persisted Queries /
Feature Flags / Cache & Backup / Analytics(月間リクエスト数・
オペレーション別レイテンシ/エラー率、`docs/cosmo-parity.md` 4a)の
10ページ管理UIが使えます(Tauri・Node.js・TypeScript
不使用、Rust→WebAssembly)。

AI HTML キャッシュを有効化して自分のアプリに載せる例・全環境変数・
全エンドポイントは **[PORTING.md](PORTING.md)** を参照してください。

### 新しいサービスを Federation に登録する(CLI)

`open-runo-cli` でGraphQLスキーマを登録すると、`open-runo-gateway`が
そのサービスをFederation経由でルーティングするようになります。

```bash
# 1. 登録したいサービスの SDL をファイルに用意
cat > users.graphql << 'EOF'
type User { id: ID! name: String! }
type Query { user(id: ID!): User }
EOF

# 2. サーバが起動している状態で登録 (stage省略時は "local")
cargo run -p open-runo-cli -- schema register \
  --service users \
  --sdl-file users.graphql \
  --stage local

# 3. 登録済みバージョンの履歴を確認
cargo run -p open-runo-cli -- schema history --service users

# 4. Federation全体の合成状況(参加サービス・型/フィールド数)を確認
cargo run -p open-runo-cli -- federation status
```

APIキーを `--api-key` で明示しない場合は自動で self-issue されるため、
ローカル開発では追加設定なしでそのまま動きます。

## ワークスペース構成（21 クレート）

| クレート | 役割 |
|----------|------|
| `open-runo-core` | 共通型（AppError / Config / Result） |
| `open-runo-router` | HTTP ゲートウェイ(管理用)・認証(KeyGuardian/RBAC/OIDC/SCIM)・監査・AI HTML キャッシュ・自己保守 |
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
| `open-runo-api-types` | 管理 API/CLI 共有の型定義 |
| `open-runo-feature-flags` | Feature Flags（決定的バケッティングによる canary ロールアウト） |
| `open-runo-rustjson` | 標準JSON完全互換の拡張パーサー（トレイリングコンマ等4拡張・`extract_path`） |
| `open-runo-appserver` | 第二のTomcat骨格（Supervisor・Dispatcher・ThreadedProxyServer） |
| `open-runo-view` | 第二のReact骨格（VNode diff/patch・hooks・SSR） |
| `open-runo-history` / `-backup` / `-observability` | 変更履歴 / バックアップ / 監視(OTLP export 対応) |

## RPoemとの「Cosmo共通コア」重複整理(2026-07-23追記)

姉妹リポジトリ[RPoem](https://github.com/aon-co-jp/RPoem)と共通の
Cosmoコアが実際にどこまで重複しているかを`diff`で調査した結果、
共通20クレート中**18クレートがbyte-for-byte完全一致**していることを
確認した。新規`scripts/sync-cosmo-core.sh`(両リポジトリに同一配置)で、
この重複状態を`check`サブコマンドで機械的に検証できるようにした
(`push`/`pull`で片方向コピーも可能)。詳細は`PORTING.md`
「RPoem⇔open-cosmoのCosmo共通コア重複整理」節を参照。

## デプロイ

同一バイナリが自前サーバー / VPS / AWS / Docker すべてで動きます。
最小構成（SQLite 1 台）から `--features full`（DUAL + Redis + ClickHouse）まで
feature フラグで選択。「マネージド版でしか使えない機能」はありません。

## インストール(open-runo-router、2026-07-23追加)

`open-web-server`(第二のApache+Nginx)とSETで使うアプリケーションサーバー
本体(`open-runo-router`、姉妹プロジェクトRPoemと同じ役割)を単体で
配布する`install.sh`(Linux、systemdサービス登録)・`install.ps1`
(Windows、サービス登録案内)・`.github/workflows/release.yml`(タグ
push時にLinux x86_64/aarch64・Windows x86_64向けバイナリを自動ビルドし
[GitHub Releases](https://github.com/aon-co-jp/open-cosmo/releases)へ添付)
を追加した。RPoem側の`install.sh`/`install.ps1`パターンをほぼそのまま
踏襲している(sibling path依存は無し、`open-runo-router`が依存する
workspace crateはすべてこのリポジトリ内)。

```
curl -fsSL https://github.com/aon-co-jp/open-cosmo/releases/latest/download/open-runo-router-x86_64-unknown-linux-gnu.tar.gz | tar xz
sudo ./install.sh
```

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
ある。open-cosmo は Federation Gateway/バックエンド側として関与しうる
(詳細は [open-web-server](https://github.com/aon-co-jp/open-web-server) の
`README.md`/`CLAUDE.md` を参照)。

## License

Apache-2.0 OR MIT(お好きな方でどうぞ)。

Contribution は [CONTRIBUTING.md](CONTRIBUTING.md) を参照してください。

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
