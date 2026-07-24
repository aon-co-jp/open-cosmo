# なぜ open-cosmo を作るのか

## REST API vs WunderGraph Cosmo (Go) vs open-cosmo (Rust) — 機能差分表

| 課題・機能 | REST API のみ | WunderGraph Cosmo (Go) | **open-cosmo (Rust + Poem)** |
|---|:---:|:---:|:---:|
| **BFF 開発が必要** | ❌ 毎回必要 | ✅ 不要 | ✅ 不要 |
| **オーバーフェッチ** | ❌ 常に発生 | ✅ GraphQL で解消 | ✅ GraphQL で解消 |
| **N+1 問題** | ❌ 発生する | ✅ Query Planning で解消 | ✅ Query Planning で解消 |
| **APIドキュメント形骸化** | ❌ Swagger がずれる | ✅ Schema が型として管理 | ✅ Schema Registry + Git履歴 |
| **エンドポイント乱立** | ❌ サービスごとにバラバラ | ✅ 1エンドポイントに集約 | ✅ 1エンドポイントに集約 |
| **認証・Security 一元管理** | ❌ サービスごとに実装 | ✅ Gateway で一元化 | ✅ Gateway で一元化 |
| **Rate Limiting** | ❌ サービスごとに実装 | ✅ あり | ✅ あり |
| **REST / gRPC / GraphQL 統合** | ❌ 手動で繋ぐ | ✅ 自動統合 | ✅ 自動統合 |
| **バージョン爆発 (/v1 /v2…)** | ❌ 永遠に積み重なる | ⚠️ 根本解決なし | ✅ **VersionlessAPI で解決** |
| **GC によるレイテンシスパイク** | ⚠️ 実装依存 | ❌ Go GC が発生する | ✅ **GC なし・安定低レイテンシ** |
| **コンパイル時メモリ安全** | ⚠️ 言語依存 | ❌ Go は実行時リスクあり | ✅ **Rust が静的保証** |
| **AI Native LLM ルーティング** | ❌ なし | ❌ なし | ✅ **Cloud / Local LLM 自動選択** |
| **DUAL DATABASE (PG + aruaru-db)** | ❌ なし | ❌ なし | ✅ **PostgreSQL + aruaru-db** |
| **Git型 Schema / DB 変更履歴** | ❌ なし | ❌ なし | ✅ **Commit・Approve・Rollback** |
| **分散自動バックアップ** | ❌ なし | ❌ なし | ✅ **S3 / VPS / Peer Node** |
| **Tauri Desktop App** | ❌ なし | ❌ なし | ✅ **TypeScript + Bootstrap 5** |
| **VersionlessAPI 互換ルール** | ❌ なし | ❌ なし | ✅ **Rename / Default / Deprecate** |
| **品質ゲート（CI 強制）** | ⚠️ 任意 | ⚠️ 任意 | ✅ **Clippy + Test + Audit 必須** |

凡例: ✅ 対応 　❌ 非対応 　⚠️ 部分的 / 実装依存

---

## 読み方

**REST API のみ** — 従来の個別マイクロサービス構成。各サービスが独自エンドポイントを持ち、BFF を手動で作り続ける。

**WunderGraph Cosmo (Go)** — GraphQL Federation で REST API 問題を大幅に解消する優れたプラットフォーム。ただし Go の GC によるレイテンシスパイク、VersionlessAPI・AI Routing・DUAL DATABASE の欠如が残る。

**open-cosmo (Rust + Poem)** — Cosmo が解決する問題をすべて引き継ぎつつ、Rust の GC なし安定性・コンパイル時安全・VersionlessAPI・AI Native Routing・DUAL DATABASE・Tauri Desktop App を追加する次世代プラットフォーム。

---

## Go (Cosmo) vs Rust (open-cosmo) — 性能・安全性の違い

| 観点 | Go (Cosmo) | Rust (open-cosmo) |
|---|---|---|
| メモリ管理 | GC（ガベージコレクション） | 所有権システム（GC なし） |
| レイテンシ特性 | GC pause により突発スパイクが起きる | GC がないため安定した低レイテンシ |
| メモリ安全性 | 実行時に nil / データ競合が発生しうる | コンパイル時に静的保証される |
| スレッド安全性 | 実行時チェック（race detector） | Send / Sync トレイトで静的保証 |
| バイナリサイズ | ランタイム込みで大きめ | ゼロコスト抽象化で小さい |
| クラウドコスト | メモリ使用量が多め | 同負荷で少ないリソース消費 |

---

## まとめ

open-cosmo は **「REST API の問題を根本解決した上で、WunderGraph Cosmo が到達できていない領域にまで踏み込む」** Rust ネイティブプラットフォームです。

詳細なアーキテクチャは [`docs/architecture.md`](architecture.md) を、API 仕様は [`docs/api-spec.md`](api-spec.md) を参照してください。
