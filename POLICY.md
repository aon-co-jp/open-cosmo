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

### 追記(入れ子の結合) / Update (nested joins)
**日本語**: 上の「ゲートウェイの動き」(3)の `@key` 結合は、現在は入れ子の段にも対応している(浅い段から順に実行)。拒否する条件は変わらない。 **English**: The `@key` join in item (3) above now also works at nested levels (executed from the shallowest level down). The rejection rules are unchanged.

## 2026-11-19 追記: 4姉妹プロジェクト(`open-cosmo`・`RPoem`・`Reaxt`・`RPoRea`)を同時に開発 / The four sister projects are developed together

### 日本語
これまでの「3姉妹」(`Reaxt`・`RPoRea`・`open-cosmo`)に、土台である **`RPoem`** を加えた **4姉妹**を、常に同時に開発します(ユーザー指示)。役割は次のとおりです。

- **`RPoem`**: 土台(アプリケーションサーバー、Tomcat 相当。管理 API は Connect / gRPC)。
- **`Reaxt`**: Rust で書く画面とサーバーアクション。Cosmo 向けのサブグラフ入口 `POST /_reaxt/graphql` を持つ。
- **`RPoRea`**: `RPoem` と `Reaxt` を合わせた土台。Rust 以外からは C ABI(`rporea-ffi`)で使う。
- **`open-cosmo`**: 複数のサブグラフをまとめるゲートウェイ(WunderGraph Cosmo 相当)。

**決まり**: どれかの接点(Cosmo のサブグラフ形式・manifest・Connect・`x-reaxt`・管理 API)を変えたら、4つの README・CLAUDE・PORTING・POLICY を同じ日に、日本語とネイティブに近い英語で更新する。最新の到達点: ゲートウェイの複数サブグラフ分割、`@key` 結合(入れ子の段にも対応)、Reaxt の `input:` リテラル対応(いずれも push 済み)。未了: `@requires` / `@provides`、変更系の後の再取得、読み取りフィールドを持つ実際の Reaxt サンプルでの通し確認。

### English
The former "three sisters" (`Reaxt`, `RPoRea`, `open-cosmo`) now include their foundation, **`RPoem`**, making **four sister projects** that are always developed together (user instruction). Their roles:

- **`RPoem`**: the foundation (an application server, comparable to Tomcat; its admin API uses Connect / gRPC).
- **`Reaxt`**: UI and server actions written in Rust, with a Cosmo-compatible subgraph entry, `POST /_reaxt/graphql`.
- **`RPoRea`**: the base that joins `RPoem` and `Reaxt`; callers other than Rust use its C ABI (`rporea-ffi`).
- **`open-cosmo`**: the gateway that combines several subgraphs (comparable to WunderGraph Cosmo).

**Rule**: when an interface between them changes (the Cosmo subgraph format, the manifest, Connect, `x-reaxt`, the admin API), update the README, CLAUDE, PORTING and POLICY of all four on the same day, in Japanese and natural English. Latest state: the gateway splits queries across subgraphs and supports `@key` joins (including nested levels), and Reaxt accepts `input:` literals (all pushed). Open: `@requires` / `@provides`, re-fetching after a mutation, and a real run with a Reaxt example that has read fields.

## 2026-11-19 追記(続き3): 実サーバー2台での「読み取りの分割」を確認、Reaxt に読み取り用アクションを追加(4姉妹共通) / Read-query split verified on two real servers; read actions added to Reaxt (shared by the four sisters)

### 日本語
- **Reaxt の規約**: メソッド名が `Get` / `List` / `Read` で始まるアクション(例: `GetCount`、`GetVisits`)は、Cosmo 向けのサブグラフで GraphQL の **Query** として公開されます。それ以外は従来どおり **Mutation** です。変更用のアクションが1つも無いときは、SDL に `Mutation` 型を出しません。`reaxt_ready` は引き続き全サブグラフで共有されます。
- **実機での確認**: カウンター(`GetCount`)とグリーター(`GetVisits`)の2台を `open-cosmo` のゲートウェイ越しに動かし、`query { c: demo_v1_Counter_GetCount g: demo_v1_Greeter_GetVisits }` が、2台に分けて実行され、`{"data":{"c":2,"g":1}}` と1つの結果にまとまることを確認しました。2台にまたがる変更系は、従来どおり「読み取り専用の問い合わせだけが分割できる」というメッセージで拒否されます。`input:` のリテラルも実機で通りました。
- **試験**: Reaxt の `reaxt-actions` は11件に成功。
- **未了**: `@requires` / `@provides`、変更系の後の再取得。

### English
- **Reaxt convention**: actions whose method name starts with `Get`, `List` or `Read` (for example `GetCount`, `GetVisits`) are exposed by the Cosmo subgraph as GraphQL **Query** fields; all other actions remain **Mutation** fields. When there are no mutating actions, the SDL has no `Mutation` type. `reaxt_ready` is still shared by every subgraph.
- **Verified on a real machine**: with the counter (`GetCount`) and greeter (`GetVisits`) servers running behind the `open-cosmo` gateway, `query { c: demo_v1_Counter_GetCount g: demo_v1_Greeter_GetVisits }` was split across the two servers and merged into one answer, `{"data":{"c":2,"g":1}}`. A mutation spanning both is still rejected, now with the message that only read-only queries can be split. `input:` literals also worked on the real servers.
- **Tests**: `reaxt-actions` passes 11 tests.
- **Open**: `@requires` / `@provides`, and re-fetching after a mutation.
