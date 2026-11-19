# API 方針(open-cosmo)

正本は `aon-co-jp/RUNO` の `API-POLICY.md`。要点: REST 全廃、ブラウザ/`curl` は Connect(`POST /<パッケージ>.<Service>/<Method>`、JSON)、サービス間は gRPC / gRPC-web。GraphQL は問い合わせ専用。

## 状況(2026-11-07、origin/main 1440213 を新規cloneして再調査)
- **自前APIとしてのRESTは0本**(2026-11-05 に RPoem の `open_runo.admin.v1.Admin` 構成へ置き換え済み。version-control の「22本」は古い数え)。
- **例外(規格が形を決める)**: `/health`(ヘルスチェック)、ACME(RFC 8555)、`/mcp`(MCP Streamable HTTP)、SCIM(RFC 7644)、`/graphql`・`/graphql/ws`・`/graphql/schema.graphql`(Cosmo 互換の問い合わせ)、`/demo`(静的デモ)、Webhook、静的ファイル。
- 旧 `/api/*` は404。`open-runo-router/src/openapi.rs`(OpenAPI生成)は gateway の旧 `/api/openapi*` 経路としては未マウントだが、`/mcp` のリソース `openapi://spec` とプロンプトが `openapi::spec()` を使うため**削除不可**(2026-11-09確認)。
- VPS では open-cosmo は稼働していない(ユーザー判断)ため、本番反映は対象外。

## 2026-11-19 追記: サブグラフ連合の方針と現状 / Subgraph federation: policy and status

### 日本語
- **方針**: 新しい REST は作らない。複数のサブグラフ(別々のサーバー)をまとめる入口は、このゲートウェイの `/graphql`(問い合わせ専用の GraphQL)とする。サブグラフ側の変更操作は Reaxt の `POST /_reaxt/graphql`(Cosmo 互換のサブグラフ入口。Connect メソッドを GraphQL の Mutation として見せる)に任せ、`x-reaxt` ヘッダーを付けて呼ぶ。
- **ゲートウェイの動き(現状)**: (1) 1つのサブグラフに届く問い合わせはそのまま転送する。(2) 読み取り専用で、最上位のフィールドが複数のサブグラフにまたがる問い合わせは、サブグラフごとに分けて同時に実行し、結果を統合する。(3) `@key` を持つ型は、サブグラフAの `id` でサブグラフBの `_entities` に問い合わせてフィールドを足す(1段のみ)。(4) 変更系が複数のサブグラフにまたがる問い合わせ、リモートとローカルが混在する問い合わせ、フラグメントやディレクティブを含む分割対象の問い合わせは、拒否する。
- **信頼の扱い**: 認証ヘッダー(`authorization`)は、許可リストにあり、安全な通信路(https またはループバック)で、取り消されていないサブグラフにだけ転送する。`cookie` は転送しない。
- **例外の追加はなし**: この追記で、REST の例外は増えていない。

### English
- **Policy**: no new REST. The single entry point that combines several subgraphs (separate servers) is this gateway's `/graphql` (query-only GraphQL). Write operations belong to each subgraph; for Reaxt that is `POST /_reaxt/graphql`, a Cosmo-compatible subgraph entry that exposes Connect methods as GraphQL mutations, called with the `x-reaxt` header.
- **Gateway behaviour (current)**: (1) A query that targets one subgraph is forwarded as is. (2) A read-only query whose top-level fields span several subgraphs is split per subgraph, run concurrently, and merged. (3) For a type with `@key`, the gateway uses the `id` from subgraph A to ask subgraph B for more fields through `_entities` (one level only). (4) Mutations that span subgraphs, queries mixing remote and local fields, and split queries that contain fragments or directives are rejected.
- **Trust**: the `authorization` header is forwarded only to a subgraph that is on the allow-list, reached over a safe channel (https or loopback), and not revoked. `cookie` is never forwarded.
- **No new exceptions**: this update adds no REST exceptions.
