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

## 2026-11-19 追記(続き4): `@external`・`@provides`・`@requires` に対応(4姉妹共通) / `@external`, `@provides` and `@requires` supported (shared by the four sisters)

### 日本語
`open-cosmo` のゲートウェイの `@key` 結合が、Apollo Federation の残りの主なディレクティブにも対応しました。

- **`@external`**: サブグラフAが `@external` と宣言したフィールドは、Aでは解決できないものとして扱い、実際に持つサブグラフBへの結合(`_entities`)の対象にします。
- **`@provides`**: 親のフィールドが `@provides(fields: "...")` で供給すると宣言したフィールドだけは、`@external` でもAに任せ、結合しません(余計な往復を減らします)。
- **`@requires`**: サブグラフBのフィールドが `@requires(fields: "a b")` で他のフィールドを必要とするとき、(1) そのフィールドが選ばれたときだけ、`a` と `b` をサブグラフAの問い合わせに足して先に取得し、(2) エンティティ表現(representation)に鍵と一緒に入れて B の `_entities` へ送ります。クライアントが選んでいない `a` と `b` は、最後に結果から取り除きます。Aが必須フィールドを供給できないときは、結合せずに従来どおり転送します。
- **設計の根拠(調査)**: Apollo の [Subgraph 仕様](https://www.apollographql.com/docs/graphos/reference/federation/subgraph-spec/)(`_entities` は表現と同じ順で返す、`@requires` のフィールドは表現に含める)と、[apollographql/router](https://github.com/apollographql/router) の計画器(必須フィールドは、要求したフィールドが選ばれたときだけ取得する修正、`FetchNode` の `requires`)を参考にしました。コードは流用せず、Rust で一から実装しています。
- **試験**: ゲートウェイ47件・federation 15件に成功(擬似サブグラフ)。
- **未了**: 必須フィールドが「さらに別のサブグラフ」にある場合(3者以上の連鎖)、`@skip` / `@include` つきの条件、変更系の後の再取得。

### English
The `@key` join in the `open-cosmo` gateway now also handles the other main Apollo Federation directives.

- **`@external`**: a field that subgraph A marks `@external` is treated as not resolvable by A, and is joined (through `_entities`) from the subgraph B that really owns it.
- **`@provides`**: a field that the parent declares as supplied by `@provides(fields: "...")` stays with A even though it is `@external`, so no extra round trip is made.
- **`@requires`**: when a field of subgraph B uses `@requires(fields: "a b")`, the gateway (1) adds `a` and `b` to the query sent to subgraph A, only when that field is selected, and (2) sends them in the entity representation together with the key to B's `_entities`. Fields the client did not select are removed from the final result. If A cannot supply a required field, the query is forwarded as before without a join.
- **Design sources (research)**: the Apollo [Subgraph specification](https://www.apollographql.com/docs/graphos/reference/federation/subgraph-spec/) (`_entities` returns entities in the order of the representations; `@requires` fields belong in the representation) and the planner in [apollographql/router](https://github.com/apollographql/router) (required fields are fetched only when the requiring field is selected; `FetchNode` has `requires`). No code was copied; this is a fresh Rust implementation.
- **Tests**: gateway 47 and federation 15 pass (fake subgraphs).
- **Open**: required fields that live in yet another subgraph (chains of three or more), `@skip` / `@include` conditions, and re-fetching after a mutation.

## 2026-11-19 追記(続き5): 変更系(mutation)が返したエンティティの結合に対応(4姉妹共通) / Joins on entities returned by a mutation (shared by the four sisters)

### 日本語
`open-cosmo` のゲートウェイが、**変更系(mutation)が返したエンティティ**にも `@key` 結合を行うようになりました。たとえば `mutation { createUser { name reviews } }` で、`createUser` は users サブグラフが実行し、`reviews`(別のサブグラフにしかないフィールド)は、返ってきた `id` を使って reviews サブグラフの `_entities`(読み取り専用)から足します。

- **変更は1回だけ**: mutation は元のサブグラフで1回だけ実行し、結合のための追加の呼び出しは読み取り専用の `_entities` だけです(二重実行しません)。試験で、users 側が `mutation` を1回だけ、reviews 側が `_entities` だけを受けたことを確認しました。
- **従来どおり拒否**: 変更系が複数のサブグラフにまたがる問い合わせは、拒否します(原子的にできないため)。
- **試験**: ゲートウェイ48件に成功(擬似サブグラフ)。
- **未了**: 必須フィールドが別のサブグラフにある3者以上の連鎖、`@skip` / `@include` つきの条件。

### English
The `open-cosmo` gateway now also performs `@key` joins on **entities returned by a mutation**. For example, in `mutation { createUser { name reviews } }` the users subgraph runs `createUser`, and `reviews` (a field only another subgraph defines) is added from the reviews subgraph's read-only `_entities`, using the returned `id`.

- **The mutation runs once**: it is executed once at its own subgraph, and the only extra calls are read-only `_entities` calls (nothing runs twice). The test confirms that the users side saw exactly one `mutation` and the reviews side saw only `_entities`.
- **Still rejected**: mutations that span several subgraphs (they cannot be made atomic).
- **Tests**: the gateway passes 48 tests (fake subgraphs).
- **Open**: chains of three or more subgraphs where a required field lives in yet another subgraph, and `@skip` / `@include` conditions.

## 2026-11-19 追記(続き6): `@skip` / `@include` と、3者以上の連鎖に対応(4姉妹共通) / `@skip` / `@include` and chains of three or more subgraphs (shared by the four sisters)

### 日本語
`open-cosmo` のゲートウェイの結合が、次の2つに対応しました。

- **`@skip(if:)` / `@include(if:)`**: 条件は、リテラルか、問い合わせの時点で分かっている変数の値で、ゲートウェイが**計画時に**判定します。除外されたフィールドは問い合わせから外し、そのフィールドのためだけに必要だった別のサブグラフへの呼び出しも行いません。これ以外のディレクティブや、値の分からない変数は、従来どおり結合せずに転送します。
- **3者以上の連鎖**: `@requires` の必須フィールドが、最初のサブグラフでも結合先でもない**さらに別のサブグラフ**にあるとき、その別のサブグラフへの結合を先に行い、得た値を結合先の表現(representation)に入れます(例: catalog → pricing で `price` を取得 → shipping の `shippingEstimate` に渡す)。結合には順位を付けて、必要な順に実行します(最大4段)。クライアントが選んでいない必須フィールドは、最後に結果から取り除きます。どのサブグラフも供給できないときは、結合せずに転送します。
- **試験**: ゲートウェイ51件に成功(擬似サブグラフ)。
- **未了**: 実際のサーバー(Reaxt 以外のサブグラフ)での `@key` / `@requires` の通し確認、`@skip` / `@include` をフラグメント内で使う場合(フラグメントは分割・結合の対象外のまま)。

### English
The join in the `open-cosmo` gateway now handles two more cases.

- **`@skip(if:)` / `@include(if:)`**: the condition is a literal or a variable whose value is already known, and the gateway decides it **at planning time**. An excluded field is dropped from the query, and a call to another subgraph that was needed only for that field is not made. Other directives, and variables with no known value, are forwarded as before, without a join.
- **Chains of three or more subgraphs**: when a `@requires` field lives in **yet another subgraph** (neither the first one nor the one being joined), the gateway first joins that third subgraph and puts the value into the representation sent onward (for example: catalog, then pricing supplies `price`, then shipping computes `shippingEstimate`). Joins get a rank and run in dependency order (up to four levels). Required fields the client did not select are removed from the final result. If no subgraph can supply a field, the query is forwarded without a join.
- **Tests**: the gateway passes 51 tests (fake subgraphs).
- **Open**: a run of `@key` / `@requires` against real servers other than Reaxt, and `@skip` / `@include` inside fragments (fragments are still outside the split and join planner).

## 2026-11-19 追記(続き7): 本物のフェデレーション実装2台で `@key` 結合を確認し、ルート型の名前の不具合を修正(4姉妹共通) / `@key` join verified against two real federation servers; root-type-name bug fixed (shared by the four sisters)

### 日本語
擬似サブグラフだけでなく、**独立した本物の GraphQL サーバー**でも確認しました。`async-graphql` のフェデレーション機能(`_service { sdl }` と `_entities` を自前で実装)で users と reviews の2台を立て、`open-cosmo` のゲートウェイ越しに `{ me { name reviews } }` を送ると、`{"name":"user-7","reviews":["review-of-7"]}` と正しく結合されました(試験はゲートウェイ52件に成功)。

- **見つかった不具合と修正**: 実際のサーバーは、ルート型の名前が `Query` とは限りません(今回は `QueryA`)。ゲートウェイは `Query` / `Mutation` 固定で探していたため、結合が働かず、元の問い合わせがそのまま転送されて失敗しました。内省(introspection)でルート型の名前を取得し、内部では `Query` / `Mutation` に正規化するようにして直しました。擬似サブグラフだけの試験では見つからなかった不具合です。
- **未了**: `@requires` と `@provides` を本物のサーバーで確認する試験(今回は `@key` のみ)、Reaxt 自身のエンティティ(`@key`)対応、フラグメント内のディレクティブ。

### English
We verified the join not only with fake subgraphs but also with **independent, real GraphQL servers**. Two servers (users and reviews) were built with `async-graphql`'s own federation support (its own `_service { sdl }` and `_entities`); sending `{ me { name reviews } }` through the `open-cosmo` gateway returned the correctly joined `{"name":"user-7","reviews":["review-of-7"]}` (the gateway passes 52 tests).

- **Bug found and fixed**: a real server's root type is not always called `Query` (here it was `QueryA`). The gateway looked for `Query` / `Mutation` by name, so no join happened and the original query was forwarded unchanged, which failed. The gateway now reads the root type names through introspection and normalizes them to `Query` / `Mutation` internally. The fake-subgraph tests alone could not have found this.
- **Open**: tests of `@requires` and `@provides` against real servers (this run covered `@key` only), entity (`@key`) support in Reaxt itself, and directives inside fragments.

## 2026-11-19 追記(続き8): 本物の SDL での `@external` / `@requires` / `@provides` の読み取りを確認、Reaxt のエンティティ対応は見送り(4姉妹共通) / Directives read from real SDL; Reaxt entities deliberately not added (shared by the four sisters)

### 日本語
- **確認**: `async-graphql` が実際に生成する SDL(`#[graphql(external)]`・`#[graphql(requires = "price")]`・`#[graphql(provides = "price")]` つき)を、ゲートウェイのモデルが正しく読むことを試験で確認しました(`@key`・`@external`・`@requires`・`@provides` の4種。ゲートウェイ53件に成功)。手書きの SDL だけでなく、実際の実装が出力する書式でも読めます。ただし、`@requires` と `@provides` を使った**結合の実行**を本物のサーバー2台で通すところまでは、まだ行っていません(実行の確認は擬似サブグラフでの試験のみ)。
- **判断(見送り)**: Reaxt 自身を `@key` のエンティティにすることは**しません**。Reaxt のアクションは `JSON` スカラーを返し、GraphQL の型(フィールドの構造)を持たないためです。型を持たせるには Reaxt のアクション定義に型情報を足す大きな設計変更が必要で、現状の用途(アクションの呼び出しと読み取り)には不要です。必要になったら、`Wire` 型から GraphQL の型を生成する形を検討します。
- **未了**: `@requires` / `@provides` を使った結合を、本物のサーバー2台で実行する試験、フラグメント内のディレクティブ。

### English
- **Verified**: the gateway model correctly reads the SDL that `async-graphql` really generates (with `#[graphql(external)]`, `#[graphql(requires = "price")]` and `#[graphql(provides = "price")]`): `@key`, `@external`, `@requires` and `@provides` (the gateway passes 53 tests). It reads the format produced by a real implementation, not only hand-written SDL. However, running a **join that uses `@requires` and `@provides`** against two real servers has not been done yet (that execution is covered only by fake-subgraph tests).
- **Decision (not added)**: Reaxt itself will **not** become a `@key` entity. Reaxt actions return the `JSON` scalar and have no GraphQL type (no field structure). Giving them one would need a large design change in the action definitions, and the current use (calling actions and reading values) does not need it. If it is ever needed, we will consider generating GraphQL types from the `Wire` types.
- **Open**: running joins that use `@requires` / `@provides` against two real servers, and directives inside fragments.


## 2026-11-19 いまの状況と、これから作る機能(4姉妹: open-cosmo・RPoem・Reaxt・RPoRea)

### 日本語(やさしい説明)
**4つは、いつも一緒に作ります。** `RPoem`は土台、`open-cosmo`は入口(玄関)、`Reaxt`は画面とサーバーの仕事を書く道具、`RPoRea`はRPoemとReaxtをくっつけたものです。1つを変えたら、ほかの3つへの影響も確かめ、4つの README・CLAUDE・PORTING・POLICY を同じ日に直します。

**もうできていること**
- `Reaxt`のサーバーが、Cosmo(複数のサーバーをまとめる仕組み)に参加できる(`POST /_reaxt/graphql`)。
- 入口(ゲートウェイ)が、1つの質問を複数のサーバーに分けて聞き、答えをまとめる。
- `@key`で、別々のサーバーが持つ同じ人・物の情報をつなぐ(入れ子や、4段までの連鎖もOK)。
- `@requires`と`@provides`(必要な項目を先に取る・持っている項目をそのまま使う)。
- `@skip`と`@include`(項目を出す・出さないの切り替え)。型条件のないフラグメントの中でも動く。
- `Reaxt`のアクションが型(項目の形)を持てる。`@key`のエンティティとして公開できる。
- 本物のサーバー2つを入口につないで、1つの質問で結合できることを確認した。

**これから作る機能(完成予定)**
1. 結合の計画(`@key`)でも、フラグメントを使えるようにする。
2. `... on 型`(型条件つきのフラグメント)に、安全な範囲で対応する。
3. 変更(書き込み)のあとに、最新の値を取り直す。
4. `Reaxt`の`Option`(あるかもしれない値)や、もっと複雑な型への対応。
5. `open-cosmo`のルーターへ、Reaxtのサーバーをもっと広く実際につなぐ試験。
6. `RPoRea`をRust以外(Pythonなど)から使う入口(C ABI)の、Mojoでの確認、Mac/Android/iPhoneへの広がり。

**進め方のきまり**: 終わっていない仕事は、**確認を取らず、自動で次々に進めます**。作ったら試験をして、本物のサーバーで確かめてから「できた」と書きます。秘密(パスワードなど)は書きません。新しいRESTは作りません。

### English (simple words, easy to read)
**We always build these four together.** `RPoem` is the base. `open-cosmo` is the front door. `Reaxt` is the tool for writing screens and server jobs. `RPoRea` joins RPoem and Reaxt. If we change one, we check the other three too, and we update the README, CLAUDE, PORTING and POLICY of all four on the same day.

**What already works**
- A `Reaxt` server can join Cosmo (a way to combine many servers) at `POST /_reaxt/graphql`.
- The front door (gateway) splits one question across many servers and puts the answers together.
- `@key` joins the same person or thing that lives in different servers (nested joins and chains up to four steps work).
- `@requires` and `@provides` (fetch needed fields first; reuse fields a server already has).
- `@skip` and `@include` (show or hide a field). They also work inside fragments that have no type condition.
- `Reaxt` actions can have types (the shape of the fields) and can be shown as `@key` entities.
- We ran two real servers behind the gateway and joined them with one question.

**What we plan to build (the finish line)**
1. Let the join planner (`@key`) use fragments too.
2. Support `... on Type` (fragments with a type condition) in a safe way.
3. Re-read the newest values after a write.
4. Support `Option` (a value that may be missing) and richer types in `Reaxt`.
5. Wider real tests that connect Reaxt servers to the `open-cosmo` router.
6. Use `RPoRea` from other languages (the C ABI): check it with Mojo, then Mac, Android and iPhone.

**How we work**: we keep going on unfinished jobs **without asking for confirmation**. We test, check on real servers, and only then say "done". We never write secrets (such as passwords). We do not add new REST APIs.


## 2026-11-19 多言語版 / Translations (8 languages; 日本語・English are above / AI翻訳、ネイティブ確認前)

### 简体中文
**这四个项目始终一起开发。** `RPoem`是地基,`open-cosmo`是大门,`Reaxt`是编写界面和服务器任务的工具,`RPoRea`把RPoem和Reaxt合在一起。改动其中一个,就要检查另外三个,并在同一天更新四个项目的README、CLAUDE、PORTING、POLICY。
- **已经完成**:Reaxt服务器可加入Cosmo(`POST /_reaxt/graphql`);网关把一个问题分给多个服务器再合并答案;`@key`连接不同服务器里的同一个人或物(可嵌套、最多4步);`@requires`、`@provides`、`@skip`、`@include`(也可在无类型条件的片段里使用);Reaxt动作可以有类型并作为`@key`实体公开;用两个真实服务器验证了一次提问合并结果。
- **计划完成**:① 连接计划支持片段(已完成);② 安全支持`... on 类型`;③ 写入后重新读取最新值;④ Reaxt支持`Option`和更复杂的类型;⑤ 更广泛的真实连接测试;⑥ 从其他语言(C ABI)使用RPoRea,先用Mojo验证,再到Mac、Android、iPhone。
- **做事规则**:未完成的工作**无需确认,自动继续**;先测试、在真实服务器上确认后才说"完成";不写入密码等机密;不新增REST。

### 繁體中文
**這四個專案永遠一起開發。** `RPoem`是地基,`open-cosmo`是大門,`Reaxt`是撰寫畫面與伺服器工作的工具,`RPoRea`把RPoem與Reaxt結合在一起。改動其中一個,就要檢查另外三個,並在同一天更新四個專案的README、CLAUDE、PORTING、POLICY。
- **已經完成**:Reaxt伺服器可加入Cosmo(`POST /_reaxt/graphql`);閘道把一個問題分給多個伺服器再合併答案;`@key`連接不同伺服器裡的同一個人或物(可巢狀、最多4步);`@requires`、`@provides`、`@skip`、`@include`(也可用於沒有型別條件的片段);Reaxt動作可以有型別並作為`@key`實體公開;以兩個真實伺服器驗證了一次提問合併結果。
- **預計完成**:① 連接計畫支援片段(已完成);② 安全支援`... on 型別`;③ 寫入後重新讀取最新值;④ Reaxt支援`Option`與更複雜的型別;⑤ 更廣泛的真實連線測試;⑥ 從其他語言(C ABI)使用RPoRea,先以Mojo驗證,再到Mac、Android、iPhone。
- **做事規則**:未完成的工作**不需確認,自動繼續**;先測試、在真實伺服器上確認後才說「完成」;不寫入密碼等機密;不新增REST。

### 한국어
**이 네 프로젝트는 항상 함께 만듭니다.** `RPoem`은 바탕, `open-cosmo`는 현관문, `Reaxt`는 화면과 서버 작업을 쓰는 도구, `RPoRea`는 RPoem과 Reaxt를 합친 것입니다. 하나를 바꾸면 나머지 셋도 확인하고, 같은 날 네 프로젝트의 README, CLAUDE, PORTING, POLICY를 고칩니다.
- **이미 된 것**: Reaxt 서버가 Cosmo에 참여(`POST /_reaxt/graphql`), 게이트웨이가 질문 하나를 여러 서버에 나눠 묻고 답을 합침, `@key`로 서로 다른 서버의 같은 사람·물건을 연결(중첩, 최대 4단계), `@requires`·`@provides`·`@skip`·`@include`(타입 조건 없는 프래그먼트 안에서도 동작), Reaxt 액션에 타입을 주고 `@key` 엔티티로 공개, 실제 서버 2대로 한 번의 질문 결합 확인.
- **만들 예정**: ① 결합 계획에서 프래그먼트 지원(완료), ② `... on 타입`의 안전한 지원, ③ 쓰기 뒤 최신 값 다시 읽기, ④ Reaxt의 `Option`과 더 복잡한 타입, ⑤ 더 넓은 실제 연결 시험, ⑥ 다른 언어(C ABI)에서 RPoRea 사용(Mojo 확인 후 Mac·Android·iPhone).
- **작업 규칙**: 끝나지 않은 일은 **확인 없이 자동으로 계속**합니다. 시험하고 실제 서버에서 확인한 뒤에만 "완료"라고 씁니다. 비밀번호 같은 비밀은 쓰지 않고, 새 REST는 만들지 않습니다.

### Deutsch
**Diese vier Projekte entwickeln wir immer zusammen.** `RPoem` ist das Fundament, `open-cosmo` die Eingangstür, `Reaxt` das Werkzeug für Oberflächen und Server-Aufgaben, `RPoRea` verbindet RPoem und Reaxt. Ändern wir eines, prüfen wir die anderen drei und aktualisieren README, CLAUDE, PORTING und POLICY aller vier am selben Tag.
- **Schon fertig**: Reaxt-Server können Cosmo beitreten (`POST /_reaxt/graphql`); das Gateway verteilt eine Frage auf mehrere Server und fügt die Antworten zusammen; `@key` verbindet dieselbe Person oder Sache aus verschiedenen Servern (verschachtelt, bis zu 4 Schritte); `@requires`, `@provides`, `@skip`, `@include` (auch in Fragmenten ohne Typbedingung); Reaxt-Aktionen haben Typen und können als `@key`-Entitäten angeboten werden; mit zwei echten Servern wurde eine Frage erfolgreich zusammengeführt.
- **Geplant**: ① Fragmente im Verbindungsplan (erledigt); ② `... on Typ` sicher unterstützen; ③ nach dem Schreiben neue Werte lesen; ④ `Option` und komplexere Typen in Reaxt; ⑤ breitere Tests mit echten Verbindungen; ⑥ RPoRea aus anderen Sprachen (C ABI) nutzen, erst Mojo, dann Mac, Android, iPhone.
- **Arbeitsregel**: Unfertiges machen wir **ohne Rückfrage automatisch weiter**. Wir testen, prüfen auf echten Servern und schreiben erst dann „fertig“. Keine Geheimnisse (z. B. Passwörter) in Dateien, keine neuen REST-APIs.

### Français
**Nous développons toujours ces quatre projets ensemble.** `RPoem` est la base, `open-cosmo` la porte d'entrée, `Reaxt` l'outil pour écrire écrans et tâches serveur, `RPoRea` réunit RPoem et Reaxt. Si l'un change, nous vérifions les trois autres et mettons à jour README, CLAUDE, PORTING et POLICY des quatre le même jour.
- **Déjà fait** : un serveur Reaxt peut rejoindre Cosmo (`POST /_reaxt/graphql`) ; la passerelle répartit une question sur plusieurs serveurs et assemble les réponses ; `@key` relie la même personne ou chose située dans des serveurs différents (imbriqué, jusqu'à 4 étapes) ; `@requires`, `@provides`, `@skip`, `@include` (aussi dans les fragments sans condition de type) ; les actions Reaxt ont des types et deviennent des entités `@key` ; une question a été assemblée avec deux vrais serveurs.
- **Prévu** : ① fragments dans le plan de jointure (fait) ; ② `... on Type` de façon sûre ; ③ relire les valeurs après une écriture ; ④ `Option` et types plus riches dans Reaxt ; ⑤ tests de connexion réelle plus larges ; ⑥ utiliser RPoRea depuis d'autres langages (C ABI) : Mojo d'abord, puis Mac, Android, iPhone.
- **Règle de travail** : ce qui n'est pas fini continue **automatiquement, sans demander**. Nous testons, vérifions sur de vrais serveurs, puis seulement écrivons « terminé ». Pas de secrets (mots de passe…) dans les fichiers, pas de nouvelle API REST.

### Español
**Siempre desarrollamos estos cuatro proyectos juntos.** `RPoem` es la base, `open-cosmo` la puerta de entrada, `Reaxt` la herramienta para escribir pantallas y tareas del servidor, y `RPoRea` une RPoem y Reaxt. Si cambiamos uno, revisamos los otros tres y actualizamos README, CLAUDE, PORTING y POLICY de los cuatro el mismo día.
- **Ya funciona**: un servidor Reaxt puede unirse a Cosmo (`POST /_reaxt/graphql`); la pasarela reparte una pregunta entre varios servidores y junta las respuestas; `@key` une la misma persona o cosa que está en servidores distintos (anidado, hasta 4 pasos); `@requires`, `@provides`, `@skip`, `@include` (también en fragmentos sin condición de tipo); las acciones de Reaxt tienen tipos y se publican como entidades `@key`; se juntó una pregunta con dos servidores reales.
- **Planeado**: ① fragmentos en el plan de unión (hecho); ② `... on Tipo` de forma segura; ③ volver a leer los valores tras escribir; ④ `Option` y tipos más ricos en Reaxt; ⑤ pruebas de conexión real más amplias; ⑥ usar RPoRea desde otros lenguajes (C ABI): primero Mojo, luego Mac, Android e iPhone.
- **Regla de trabajo**: lo que falta se sigue **automáticamente, sin pedir confirmación**. Probamos, verificamos en servidores reales y solo entonces escribimos «terminado». Sin secretos (contraseñas) en los archivos y sin nuevas APIs REST.

### Português
**Sempre desenvolvemos estes quatro projetos juntos.** `RPoem` é a base, `open-cosmo` é a porta de entrada, `Reaxt` é a ferramenta para escrever telas e tarefas do servidor, e `RPoRea` une RPoem e Reaxt. Se mudamos um, verificamos os outros três e atualizamos README, CLAUDE, PORTING e POLICY dos quatro no mesmo dia.
- **Já funciona**: um servidor Reaxt pode entrar no Cosmo (`POST /_reaxt/graphql`); o gateway divide uma pergunta entre vários servidores e junta as respostas; `@key` liga a mesma pessoa ou coisa que está em servidores diferentes (aninhado, até 4 passos); `@requires`, `@provides`, `@skip`, `@include` (também em fragmentos sem condição de tipo); as ações do Reaxt têm tipos e viram entidades `@key`; uma pergunta foi juntada com dois servidores reais.
- **Planejado**: ① fragmentos no plano de junção (feito); ② `... on Tipo` de forma segura; ③ reler os valores após escrever; ④ `Option` e tipos mais ricos no Reaxt; ⑤ testes de conexão real mais amplos; ⑥ usar o RPoRea de outras linguagens (C ABI): primeiro Mojo, depois Mac, Android e iPhone.
- **Regra de trabalho**: o que falta continua **automaticamente, sem pedir confirmação**. Testamos, verificamos em servidores reais e só então escrevemos «concluído». Sem segredos (senhas) nos arquivos e sem novas APIs REST.

### Русский
**Эти четыре проекта мы всегда делаем вместе.** `RPoem` — основа, `open-cosmo` — входная дверь, `Reaxt` — инструмент для экранов и серверных задач, `RPoRea` соединяет RPoem и Reaxt. Если меняем один, проверяем остальные три и в тот же день обновляем README, CLAUDE, PORTING и POLICY всех четырёх.
- **Уже работает**: сервер Reaxt может присоединиться к Cosmo (`POST /_reaxt/graphql`); шлюз делит один вопрос между серверами и собирает ответы; `@key` связывает одного и того же человека или предмет из разных серверов (вложенно, до 4 шагов); `@requires`, `@provides`, `@skip`, `@include` (в том числе во фрагментах без условия типа); действия Reaxt имеют типы и открываются как сущности `@key`; на двух настоящих серверах один вопрос успешно объединён.
- **Планируется**: ① фрагменты в плане соединения (готово); ② безопасная поддержка `... on Тип`; ③ перечитывание значений после записи; ④ `Option` и более сложные типы в Reaxt; ⑤ более широкие проверки с реальными соединениями; ⑥ использование RPoRea из других языков (C ABI): сначала Mojo, затем Mac, Android и iPhone.
- **Правило работы**: незавершённое мы продолжаем **автоматически, не спрашивая подтверждения**. Сначала проверяем на настоящих серверах и только потом пишем «готово». Секреты (пароли) в файлы не пишем, новых REST-API не создаём.


## 2026-11-19 リミット前のチェックポイント(4姉妹・日本語、別アカウントでの再開用)
**到達点(すべてpush済み、試験成功)**
- ゲートウェイ(open-cosmo `remote_subgraphs.rs`): 複数サブグラフへの分割、`@key`結合(入れ子・4段連鎖)、`@requires`/`@provides`、`@skip`/`@include`、型条件なしのインラインフラグメントの展開、`... on 型`(親と同じ型のときだけ展開)。ゲートウェイ試験57件成功。
- Reaxt(`reaxt-actions`): アクションが型を持つ(`Wire::gql_type`・`wire_struct!`)、`@key`エンティティ(`ModuleActions::entity`)、`_entities`、選択の射影(別名・`__typename`・`... on`)、`Option<T>`(null可)、`f64`(Float)、JSONに無いフィールドは`Option`なら`None`。試験15件成功。
- 実機: `serve`と`serve_greeter`(どちらも型付き`Visitor`)をゲートウェイ(`OPEN_RUNO_SUBGRAPHS="名前=URL,..."`)の後ろに置き、1つの質問で`visits`と`count`が`@key`で結合されることを確認。2サーバーにまたがる変更は仕様どおり拒否。
- 文書: 4リポジトリとrunoのREADME/CLAUDE/PORTING/POLICYに、現状・完成予定・きまりを日英と8言語(AI翻訳、ネイティブ確認前)で記載済み。

**未了(この順で、確認不要で自動で進める)**
1. 変更(書き込み)の後に最新の値を取り直す機能。
2. 名前付きフラグメント(`...F`)と、型が違う`... on 型`(インターフェース/ユニオン)への安全な対応。
3. Reaxtの複雑な型(`Option`の入れ子、`HashMap`など)、`Wire`の対応型の拡張。
4. open-cosmoのルーターへReaxtをさらに広く実際につなぐ試験。
5. RPoRea(C ABI)をMojoで確認、Mac・Android・iPhoneへ。
6. クラウドでの軽い仕事の定期実行(2時間ごと): **GitHubをClaudeアカウントに接続**(https://claude.ai/connect-github)してから作る。

**注意**: Reaxtの型の登録(レジストリ)はプロセス内で共有。並列試験では`JSON`か型付きかの表記が揺れるので、SDL試験は`(input: JSON):`までで比べる。ゲートウェイの`OPEN_RUNO_SUBGRAPHS`は`名前=URL`形式。新しいRESTは作らない。秘密は書かない。

**再開用メッセージ**: 前の続きです。`F:\runo\PORTING.md`末尾「リミット前のチェックポイント(4姉妹)」を読んでから、open-cosmo・RPoem・Reaxt・RPoRea の未了を、確認なしで自動で進めてください。区切りごとにREADME/CLAUDE/PORTING/POLICYを10か国語で更新し、commitしてpushします。返答は日本語のみ。


## 2026-11-19 まとめと再開のしかた / Summary and how to resume (日本語・English)

### 日本語(やさしい説明)
**今日できたこと**
- Reaxtのサーバーを、入口(open-cosmo)の後ろに並べて使えるようになりました。入口は、1つの質問を複数のサーバーに分けて聞き、答えを1つにまとめます。
- 別々のサーバーにある「同じ人・同じ物」の情報を、`@key`でつなげます。`@requires`・`@provides`・`@skip`・`@include`・フラグメントにも対応しました。
- Reaxtの仕事(アクション)に「型」(中身の形)を持たせられるようになりました。値が無いかもしれない型(`Option`)や小数(`f64`)も使えます。
- 本物のサーバー2つで、1回の質問に両方の答えが入ることを確かめました。試験はゲートウェイ57件・Reaxt15件が成功しています。

**まだ終わっていないこと(この順に進めます)**
1. 書き込みのあと、最新の値を取り直す。
2. 名前付きフラグメントや、型が違う`... on 型`への安全な対応。
3. Reaxtでもっと複雑な型を使えるようにする。
4. open-cosmoのルーターで、もっと広く実際につなぐ試験。
5. RPoReaをMojo・Mac・Android・iPhoneから使えるか確かめる。
6. 軽い仕事は、クラウドで2時間ごとに自動実行する(先にGitHubをClaudeにつなぐ: https://claude.ai/connect-github)。

**再開のしかた**: 新しい会話で「`F:\runo\PORTING.md`の末尾を読んで、4姉妹(open-cosmo・RPoem・Reaxt・RPoRea)の未了を確認なしで自動で進めて」と伝えてください。**進め方のきまり**: 確認は取らずに進める/試験して本物で確かめてから「できた」と書く/秘密は書かない/新しいRESTは作らない。

### English (natural, plain wording)
**What we got done today**
- Reaxt servers can now sit behind the front door (open-cosmo). The gateway splits one question across several servers and merges the answers into one.
- `@key` links the same person or item that lives on different servers. `@requires`, `@provides`, `@skip`, `@include` and fragments work too.
- Reaxt actions can now carry types (the shape of their data), including optional values (`Option`) and decimals (`f64`).
- With two real servers we confirmed that a single question returns data from both. All tests pass: 57 in the gateway and 15 in Reaxt.

**What is still open (we will go in this order)**
1. Re-read the latest values after a write.
2. Safe support for named fragments and for `... on Type` with a different type.
3. Richer types in Reaxt.
4. Wider real-world tests that connect Reaxt to the open-cosmo router.
5. Check that RPoRea works from Mojo, Mac, Android and iPhone.
6. Run the light jobs in the cloud every two hours (first connect GitHub to Claude: https://claude.ai/connect-github).

**How to resume**: in a new conversation, say "Read the end of `F:\runo\PORTING.md`, then keep working through the open items of the four sister projects (open-cosmo, RPoem, Reaxt, RPoRea) without asking for confirmation." **Working rules**: keep going without asking; test and verify on real systems before saying "done"; never write secrets; do not add new REST APIs.


## 2026-11-19 最終まとめ(リミット前) / Final summary before the limit (日本語・English)

### 日本語(やさしい説明)
**さっき終わったこと**
- **名前付きフラグメント**(`...F`)に対応しました。ゲートウェイは、問い合わせの中の`...F`を、`... on 型 { }`の形に置きかえてから計画します。型が親と同じなら展開し、`@skip`/`@include`も効きます。ぐるぐる回る参照(循環)は、無限ループにならず、そのまま転送します。
- **Reaxtのサーバー**も、名前付きフラグメント・型なしの`... { }`・`@skip`/`@include`(変数もOK)を読めるようになりました。これで、全部の項目が除外されて「分割せずそのまま転送」になった場合も、ちゃんと動きます。
- 本物のサーバー2つをゲートウェイの後ろに置いて、`...F @include(if: $x)`で、`true`なら`visits`と`count`が結合されて返り、`false`なら`id`だけが返ることを確かめました。
- 試験: ゲートウェイ58件、Reaxt16件が成功。

**まだ終わっていないこと**
1. 書き込みのあと、最新の値を取り直す。
2. 型が違う`... on 型`(インターフェース/ユニオン)への安全な対応。
3. Reaxtでもっと複雑な型(`Option`の入れ子、`HashMap`など)を使えるようにする。
4. open-cosmoのルーターで、もっと広く実際につなぐ試験。
5. RPoReaをMojo・Mac・Android・iPhoneから使えるか確かめる。
6. 軽い仕事のクラウド定期実行(先にGitHubをClaudeにつなぐ)。

**再開のしかた**: 新しい会話で「`F:\runo\PORTING.md`の末尾を読んで、4姉妹の未了を確認なしで自動で進めて」と伝えてください。

### English (natural, plain wording)
**What we just finished**
- **Named fragments** (`...F`) are now supported. The gateway rewrites each `...F` into `... on Type { }` before planning. When the type matches the parent it is flattened, and `@skip`/`@include` work. A circular reference does not loop forever; the query is simply forwarded as it is.
- **Reaxt servers** can now read named fragments, untyped `... { }` and `@skip`/`@include` (variables too). So even when every field is excluded and the query is forwarded unsplit, it still works.
- With two real servers behind the gateway, `...F @include(if: $x)` returned `visits` and `count` joined together for `true`, and only `id` for `false`.
- Tests: 58 in the gateway and 16 in Reaxt all pass.

**What is still open**
1. Re-read the latest values after a write.
2. Safe support for `... on Type` with a different type (interfaces and unions).
3. Richer Reaxt types (nested `Option`, `HashMap`, and so on).
4. Wider real-world tests that connect Reaxt to the open-cosmo router.
5. Check that RPoRea works from Mojo, Mac, Android and iPhone.
6. Run the light jobs in the cloud on a schedule (first connect GitHub to Claude).

**How to resume**: in a new conversation, say "Read the end of `F:\runo\PORTING.md`, then keep working through the open items of the four sister projects without asking for confirmation."


## 2026-11-19 追記(リミット前の最後) / Last addition before the limit (日本語・English)

### 日本語(やさしい説明)
- Reaxtのアクションで使える整数の種類を増やしました(`u64`・`i8`・`i16`・`isize`を追加)。範囲の外の数(たとえば`i8`に`300`)や、負の数を`u64`に入れようとすると、エラーになります。試験は17件成功です。
- ゲートウェイの試験は58件成功のままです。名前付きフラグメント(`...F`)と`@skip`/`@include`は、本物のサーバー2つで確かめ済みです。
- **残りの仕事**: ①書き込みのあとに最新の値を取り直す ②型が違う`... on 型`(インターフェース/ユニオン) ③Reaxtのもっと複雑な型 ④open-cosmoのルーターでの広い実機試験 ⑤RPoReaをMojo・Mac・Android・iPhoneから使う ⑥軽い仕事のクラウド定期実行(先にGitHubをClaudeにつなぐ)。
- 再開のしかた: 新しい会話で「`F:\runo\PORTING.md`の末尾を読んで、4姉妹の未了を確認なしで自動で進めて」と伝えてください。

### English (natural, plain wording)
- Reaxt actions can now use more integer types (`u64`, `i8`, `i16` and `isize` were added). Out-of-range numbers (for example `300` as an `i8`) and negative numbers for a `u64` are rejected with an error. 17 tests pass.
- The gateway still passes its 58 tests. Named fragments (`...F`) and `@skip`/`@include` were checked with two real servers.
- **Still open**: (1) re-read the latest values after a write; (2) `... on Type` with a different type (interfaces and unions); (3) richer Reaxt types; (4) wider real-world tests with the open-cosmo router; (5) use RPoRea from Mojo, Mac, Android and iPhone; (6) run the light jobs in the cloud on a schedule (first connect GitHub to Claude).
- How to resume: in a new conversation, say "Read the end of `F:\runo\PORTING.md`, then keep working through the open items of the four sister projects without asking for confirmation."


## 2026-11-19 追記: 書き込みのあとの取り直し(実機確認) / Re-reading after a write (verified)

### 日本語(やさしい説明)
- 書き込み(変更)が型付きの結果を返すとき、**同じ1回の質問で、別のサーバーの最新の値も受け取れる**ことを、本物のサーバー2つで確かめました。例: `mutation { Touch(input:{id:"a1"}) { id visits count } }` は、書き込み(`visits`)を**1回だけ**実行し、そのあと`count`を別のサーバーから読みます(2回続けて呼ぶと`visits`は1、2と増えます)。
- 仕組みは、これまでの「変更は1回だけ実行して、読み取りだけをほかのサーバーへつなぐ」計画です。新しい仕組みを足さなくても、取り直しの目的を満たしています。`serve_greeter`に`Touch`の例を足しました。
- この項目(書き込み後の取り直し)は完了にします。残りは、型が違う`... on 型`、Reaxtのもっと複雑な型、広い実機試験、Mojo/Mac/Android/iPhone、クラウドの定期実行(先にGitHubをClaudeにつなぐ)です。

### English (natural, plain wording)
- When a write returns a typed result, **one single request can also bring back the latest values from another server**. We checked this with two real servers. Example: `mutation { Touch(input:{id:"a1"}) { id visits count } }` runs the write (`visits`) **exactly once**, then reads `count` from the other server (calling it twice makes `visits` go 1, then 2).
- It works through the existing plan: run the mutation once and join only read-only fields from other servers. No new mechanism was needed to meet the goal. We added a `Touch` example to `serve_greeter`.
- We mark this item (re-reading after a write) as done. Still open: `... on Type` with a different type, richer Reaxt types, wider real-world tests, Mojo/Mac/Android/iPhone, and the scheduled cloud runs (first connect GitHub to Claude).


## 2026-11-19 リミット前の最終まとめ(2回目) / Final summary before the limit, part 2 (日本語・English)

### 日本語(やさしい説明)
**終わったこと**
- **`... on 型`**: スキーマに「インターフェース」や「ユニオン」が1つも無いときは、型が違う`... on 別の型`は絶対に当てはまらないので、安全に取り除けるようにしました(`SubgraphModel.has_abstract`で判定)。インターフェース/ユニオンがあるときは、これまでどおり通常の転送にします。ゲートウェイ試験59件・federation試験16件が成功。
- **Androidスマホでの実機試験**: `librporea_ffi.so`(aarch64)をスマホ(A401OP)に入れ、Cのテストプログラム(`RPoRea/bindings/c/android_test.c`)で、サーバー起動・Connectの呼び出し(200)・ファイル配信・拒否(403)・GETは405(RESTなし)を確認し、すべて成功しました。
- **書き込み後の取り直し**: 実機で確認済み(前の節)。

**まだ終わっていないこと**
1. **Mojo**: WSLにMojoを入れて、`RPoRea`のC ABIを呼ぶ確認と`bindings/mojo`の作成(導入中。入ったら、動かして書く)。
2. Reaxtのもっと複雑な型(`Option`の入れ子、`HashMap`など)。
3. open-cosmoのルーターでの、もっと広い実機試験。
4. Mac・iPhoneでの確認。
5. 軽い仕事のクラウド定期実行(先にGitHubをClaudeにつなぐ)。

**再開のしかた**: 新しい会話で「`F:\runo\PORTING.md`の末尾を読んで、4姉妹の未了を確認なしで自動で進めて」と伝えてください。

### English (natural, plain wording)
**What is done**
- **`... on Type`**: when a schema has no interface or union at all, a fragment on a different type can never match, so it is now safely dropped (decided by `SubgraphModel.has_abstract`). When interfaces or unions exist, the query is forwarded unsplit as before. 59 gateway tests and 16 federation tests pass.
- **Real Android phone test**: we put `librporea_ffi.so` (aarch64) on the phone (A401OP) and ran a C test program (`RPoRea/bindings/c/android_test.c`). Starting the server, a Connect call (200), static assets, a denial (403) and GET returning 405 (no REST) all passed.
- **Re-reading after a write**: verified on real servers (previous section).

**What is still open**
1. **Mojo**: install Mojo in WSL, check that it can call RPoRea's C ABI, and write `bindings/mojo` (the install is running; once it finishes we run and write it).
2. Richer Reaxt types (nested `Option`, `HashMap`, and so on).
3. Wider real-world tests with the open-cosmo router.
4. Checks on Mac and iPhone.
5. Scheduled cloud runs for light jobs (first connect GitHub to Claude).

**How to resume**: in a new conversation, say "Read the end of `F:\runo\PORTING.md`, then keep working through the open items of the four sister projects without asking for confirmation."


## 2026-11-20 Mojoでの確認(WSL) / Mojo check (WSL) (日本語・English)

### 日本語(やさしい説明)
- **Mojo 1.2.0をWSL(Ubuntu)に入れました**(`pip install mojo`、仮想環境`~/mojo-venv`)。MojoはWindowsでは直接動かないので、WSLで使います。
- **MojoからRPoReaのC ABIを実際に呼ぶ試験**(`RPoRea/bindings/mojo/test_rporea.mojo`)が成功しました: ライブラリ読み込み → バージョン取得 → ビルダー作成 → ファイル登録 → サーバー起動 → HTTPで`hello from mojo`を取得(200) → 未登録のメソッドは404。実行のしかた: `RPOREA_LIB=/root/rporea-target/release/librporea_ffi.so mojo test_rporea.mojo`(WSL)。
- **Mojo 1.2での書き方のコツ**: ポインタは`Int`で渡す(`Int(s.unsafe_ptr())`)。`len(String)`は使えないので`byte_length()`。Pythonの値は`Int(py=...)`・`String(py=...)`で変換する。
- **まだ**: Mojoの関数をC側のコールバックとして渡す部分(`rporea_method`)は未了。Mac・iPhoneの確認も未了。
- Androidスマホ(A401OP)での実機試験は、前の節のとおり成功済みです。

### English (natural, plain wording)
- **Mojo 1.2.0 is installed in WSL (Ubuntu)** (`pip install mojo`, virtual environment `~/mojo-venv`). Mojo does not run natively on Windows, so we use it through WSL.
- **A test that calls RPoRea's C ABI from Mojo** (`RPoRea/bindings/mojo/test_rporea.mojo`) passes: load the library, read the version, create a builder, register a file, start the server, fetch `hello from mojo` over HTTP (200), and get 404 for an unregistered method. Run it in WSL with `RPOREA_LIB=/root/rporea-target/release/librporea_ffi.so mojo test_rporea.mojo`.
- **Tips for Mojo 1.2**: pass pointers as `Int` (`Int(s.unsafe_ptr())`); `len(String)` is not allowed, so use `byte_length()`; convert Python values with `Int(py=...)` and `String(py=...)`.
- **Still open**: passing a Mojo function as a C callback (`rporea_method`), and the Mac and iPhone checks.
- The real-phone test on the Android device (A401OP) passed earlier (see the previous section).


## 2026-11-20 追記: Mojoの関数をコールバックとして渡せた / A Mojo function works as a C callback (日本語・English)

### 日本語(やさしい説明)
- **Mojoで書いた関数を、RPoReaのメソッド(`rporea_method`)として登録できました。** `RPoRea/bindings/mojo/test_callback.mojo` で、`POST /demo.v1.Echo/Say` に`{"a":1}`を送ると、Mojoの関数が`{"echo":{"a":1}}`を返し、200になることを確かめました。
- 書き方のコツ: コールバックは`def 名前(user: Int, body: Int, blen: UInt, dst: Int, cap: UInt) abi("C") -> Int`と書きます(`out`はMojoの予約語なので`dst`)。ポインタは`Int`で受け取り、`UnsafePointer[UInt8, MutAnyOrigin](unsafe_from_address=...)`で読み書きします。登録のときは、関数をそのまま引数に渡します。
- 実行のしかた(WSL): `RPOREA_LIB=/root/rporea-target/release/librporea_ffi.so mojo test_callback.mojo`。
- **まだ**: Mac・iPhoneでの確認、Reaxtのもっと複雑な型、open-cosmoでの広い実機試験、クラウドの定期実行(先にGitHubをClaudeにつなぐ)。

### English (natural, plain wording)
- **A function written in Mojo can now be registered as an RPoRea method (`rporea_method`).** In `RPoRea/bindings/mojo/test_callback.mojo`, a `POST /demo.v1.Echo/Say` with `{"a":1}` returned `{"echo":{"a":1}}` with status 200, produced by the Mojo function.
- Tips: write the callback as `def name(user: Int, body: Int, blen: UInt, dst: Int, cap: UInt) abi("C") -> Int` (`out` is a reserved word in Mojo, so use `dst`). Receive pointers as `Int` and read or write them with `UnsafePointer[UInt8, MutAnyOrigin](unsafe_from_address=...)`. When registering, pass the function itself as the argument.
- Run it in WSL: `RPOREA_LIB=/root/rporea-target/release/librporea_ffi.so mojo test_callback.mojo`.
- **Still open**: the Mac and iPhone checks, richer Reaxt types, wider real-world tests with open-cosmo, and the scheduled cloud runs (first connect GitHub to Claude).


## 2026-11-20 追記: Mac・iPhone・Androidの確認(CI) / Mac, iPhone and Android checks (CI) (日本語・English)

### 日本語(やさしい説明)
- **RPoReaにCI(GitHub Actions)を追加しました**(`RPoRea/.github/workflows/ci.yml`)。結果は**5つすべて成功**です。
  - Linux・macOS・Windows: ビルド、テスト、そして**PythonからC ABIを実際に呼ぶ試験**(macOSでは`.dylib`)。
  - iPhone(iOS): 実機用(`aarch64-apple-ios`)とシミュレーター用(`aarch64-apple-ios-sim`)のビルド。
  - Android(NDK): `aarch64-linux-android`のビルド。
- つまり、**Macでの動作(Python経由の実呼び出し)とiPhone向けのビルドが、CIで確認できました**。Androidはスマホの実機試験も成功済みです。iPhoneの実機・Macの実機での目視確認は、実機が無いのでできていません。
- CIがつまずいた理由と直し方: RPoReaは兄弟のリポジトリを相対パスで使うので、CIでも`Reaxt`・`RPoem`・`RS-JSON`・`open-web-server`・`RS-SmartTCP`・`open-raid-z`・`open-cpu`・`open-cuda`・`open-directx`を同じ親フォルダーへ取得します(`working-directory: RPoRea`)。新しい兄弟リポジトリを使うようになったら、`ci.yml`に足してください。
- **まだ**: クラウドの定期実行(先にGitHubをClaudeにつなぐ)。iPhone・Macの実機での確認。

### English (natural, plain wording)
- **We added CI (GitHub Actions) to RPoRea** (`RPoRea/.github/workflows/ci.yml`), and **all five jobs pass**.
  - Linux, macOS and Windows: build, tests, and a **Python test that really calls the C ABI** (the `.dylib` on macOS).
  - iPhone (iOS): builds for devices (`aarch64-apple-ios`) and the simulator (`aarch64-apple-ios-sim`).
  - Android (NDK): a build for `aarch64-linux-android`.
- So **running on a Mac (a real call through Python) and building for iPhone are now confirmed in CI**. Android also passed the real-phone test. We have not checked by eye on a real iPhone or a real Mac, because we have no such devices.
- Why CI tripped, and the fix: RPoRea uses sibling repositories through relative paths, so CI also checks out `Reaxt`, `RPoem`, `RS-JSON`, `open-web-server`, `RS-SmartTCP`, `open-raid-z`, `open-cpu`, `open-cuda` and `open-directx` into the same parent folder (`working-directory: RPoRea`). If RPoRea starts using another sibling repository, add it to `ci.yml`.
- **Still open**: scheduled cloud runs (first connect GitHub to Claude), and real-device checks on iPhone and Mac.


## 2026-11-20 現状と完成予定 / Status and planned work (日本語・English)

### 日本語(やさしい説明)
**現状(実機で確認済み)**
- 4姉妹(open-cosmo・RPoem・Reaxt・RPoRea)は、いつも一緒に作っています。
- ゲートウェイ(open-cosmo)は、複数のサーバーに質問を分けて、答えを1つにまとめます。`@key`・`@requires`・`@provides`・`@skip`・`@include`・名前付きフラグメント・書き込みのあとの取り直しに対応しています。試験はゲートウェイ59件、Reaxt 22件、本物のサーバーを並べた通し試験21項目が成功しています。
- **Reaxt 以外の実装とも結合できます**: Apollo(Node.js)と Rust の async-graphql のサブグラフを、Reaxt と並べて、分割と `@key` 結合が動くことを確かめました。
- Reaxt のアクションは、`Option`・`Vec`・`HashMap`・`f32`・`f64`・各種整数・`Box<T>`・2〜3個のタプルを使えます(v0.1.2 まで公開済み、それ以降は `main`)。
- RPoRea は、Python・Mojo・Android(実機)から呼べます。Mac と iPhone は CI での確認までです。
- 全公開リポジトリと非公開9件に、最新の `.md` とインストーラー一覧を出す仕組み(`docs-latest`)が入っています(日本時間 10時・12時・22時ごろ)。

**完成予定(未了)**
1. Mac・iPhone の実機確認(実機がありません)。
2. 4つ以上のタプルへの対応(必要になってから)。
3. クラウドでの定期実行(GitHub を Claude につなぐと作れます)。
4. `docs-latest` の定期実行(cron)が実際に動いたかの確認(日本時間 22:07 以降)。
5. 次の `Reaxt` リリース(`f32`・`Box<T>`・タプルを入れる)。
- 対象外にしたもの: `Option` の入れ子(`null` で段が区別できません)。

**決まり**: 終わっていない仕事は確認せず自動で進めます。試験して本物で確かめてから「できた」と書きます。秘密は書きません。新しい REST は作りません。Node.js は、ユーザーが許可した試験(Apollo の相互運用)だけで使います。

### English (plain words)
**Where we are (checked on real systems)**
- We always build the four sisters (open-cosmo, RPoem, Reaxt, RPoRea) together.
- The gateway (open-cosmo) splits a question across several servers and merges the answers. It supports `@key`, `@requires`, `@provides`, `@skip`, `@include`, named fragments and re-reading after a write. 59 gateway tests, 22 Reaxt tests and a 21-check end-to-end run with real servers all pass.
- **It also joins with non-Reaxt implementations**: we ran an Apollo (Node.js) subgraph and a Rust async-graphql subgraph next to Reaxt, and both splitting and `@key` joins worked.
- Reaxt actions can use `Option`, `Vec`, `HashMap`, `f32`, `f64`, the integer types, `Box<T>` and 2- to 3-element tuples (released up to v0.1.2; later work is on `main`).
- RPoRea can be called from Python, Mojo and Android (real phone). Mac and iPhone are checked in CI only.
- Every public repository and 9 private ones have the `docs-latest` workflow, which shows the latest `.md` files and an installer list (about 10:00, 12:00 and 22:00 Japan time).

**Planned (still open)**
1. Real-device checks on Mac and iPhone (we have no devices).
2. Tuples of four or more (when needed).
3. Scheduled cloud runs (possible once GitHub is connected to Claude).
4. Confirming that the `docs-latest` schedule (cron) really fires (after 22:07 Japan time).
5. The next `Reaxt` release (to ship `f32`, `Box<T>` and tuples).
- Left out on purpose: nested `Option` (`null` cannot tell the levels apart).

**Rules**: we go on with unfinished work without asking; we test and verify on real systems before saying "done"; we never write secrets; we add no new REST APIs; Node.js is used only for tests the owner allowed (the Apollo interop).

## 2026-11-20 現状と完成予定(8言語) / Status and planned work in 8 more languages (AI翻訳・ネイティブ確認前 / AI-translated, not yet native-checked)
日本語・English は直前の節「現状と完成予定 / Status and planned work」にあります。Japanese and English are in the section just above.

### 简体中文
**现状(已在真实环境确认)**:四个姐妹项目(open-cosmo、RPoem、Reaxt、RPoRea)始终一起开发。网关把一个问题分给多个服务器并合并答案,支持 `@key`、`@requires`、`@provides`、`@skip`、`@include`、具名片段和写入后重新读取。网关测试59项、Reaxt 测试22项、用真实服务器的端到端21项全部通过。已确认可与非 Reaxt 的实现(Apollo、Rust 的 async-graphql)联合。Reaxt v0.1.3 已发布(`f32`、`Box<T>`、最多6个元素的元组)。
**计划完成(未完成)**:① Mac 与 iPhone 真机确认(没有设备);② 云端定时执行(GitHub 已连接,但需在新会话或网页中注册,推送还需安装 Claude GitHub App);③ 确认 `docs-latest` 的定时(cron)确实触发。有意不支持:嵌套 `Option`。
**规则**:未完成的工作无需确认自动继续;在真实环境验证后才写"完成";不写入密码等机密;不新增 REST;Node.js 仅用于用户允许的测试。

### 繁體中文
**現況(已在真實環境確認)**:四個姊妹專案(open-cosmo、RPoem、Reaxt、RPoRea)一律一起開發。閘道把一個問題分給多個伺服器並合併答案,支援 `@key`、`@requires`、`@provides`、`@skip`、`@include`、具名片段與寫入後重新讀取。閘道測試59項、Reaxt 測試22項、以真實伺服器進行的端到端21項皆通過。已確認可與非 Reaxt 的實作(Apollo、Rust 的 async-graphql)聯合。Reaxt v0.1.3 已發布(`f32`、`Box<T>`、最多6個元素的元組)。
**預計完成(未完成)**:① Mac 與 iPhone 實機確認(沒有裝置);② 雲端定時執行(GitHub 已連接,但需在新工作階段或網頁中登錄,推送還需安裝 Claude GitHub App);③ 確認 `docs-latest` 的排程(cron)確實觸發。刻意不支援:巢狀 `Option`。
**規則**:未完成的工作不需確認自動繼續;在真實環境驗證後才寫「完成」;不寫入密碼等機密;不新增 REST;Node.js 僅用於使用者允許的測試。

### 한국어
**현황(실제 환경에서 확인)**: 네 자매 프로젝트(open-cosmo, RPoem, Reaxt, RPoRea)는 항상 함께 개발합니다. 게이트웨이는 질문 하나를 여러 서버에 나눠 묻고 답을 합치며, `@key`, `@requires`, `@provides`, `@skip`, `@include`, 이름 있는 프래그먼트, 쓰기 뒤 다시 읽기를 지원합니다. 게이트웨이 시험 59건, Reaxt 시험 22건, 실제 서버로 한 종단 간 시험 21항목이 모두 통과했습니다. Reaxt 외의 구현(Apollo, Rust의 async-graphql)과도 결합됨을 확인했습니다. Reaxt v0.1.3을 공개했습니다(`f32`, `Box<T>`, 최대 6개 요소의 튜플).
**완성 예정(미완료)**: ① Mac·iPhone 실기기 확인(기기가 없음), ② 클라우드 정기 실행(GitHub는 연결했으나 새 세션이나 웹에서 등록해야 하고, push에는 Claude GitHub App 설치가 필요), ③ `docs-latest`의 정기 실행(cron)이 실제로 동작하는지 확인. 의도적으로 제외: 중첩 `Option`.
**규칙**: 끝나지 않은 일은 확인 없이 자동으로 계속합니다. 실제 환경에서 확인한 뒤에만 "완료"라고 씁니다. 비밀번호 같은 비밀은 쓰지 않고, 새 REST는 만들지 않으며, Node.js는 사용자가 허가한 시험에만 씁니다.

### Deutsch
**Stand (auf echten Systemen geprüft)**: Die vier Schwesterprojekte (open-cosmo, RPoem, Reaxt, RPoRea) entwickeln wir immer gemeinsam. Das Gateway verteilt eine Frage auf mehrere Server und fügt die Antworten zusammen; es unterstützt `@key`, `@requires`, `@provides`, `@skip`, `@include`, benannte Fragmente und das erneute Lesen nach einem Schreibvorgang. 59 Gateway-Tests, 22 Reaxt-Tests und ein End-to-End-Lauf mit 21 Prüfungen auf echten Servern bestehen. Die Zusammenarbeit mit Implementierungen außerhalb von Reaxt (Apollo, Rusts async-graphql) ist bestätigt. Reaxt v0.1.3 ist veröffentlicht (`f32`, `Box<T>`, Tupel mit bis zu 6 Elementen).
**Geplant (offen)**: ① Prüfung auf echten Mac- und iPhone-Geräten (keine Geräte vorhanden); ② zeitgesteuerte Cloud-Läufe (GitHub ist verbunden, muss aber in einer neuen Sitzung oder im Web angelegt werden; zum Pushen braucht es zudem die Claude GitHub App); ③ Bestätigung, dass der Zeitplan (cron) von `docs-latest` wirklich auslöst. Bewusst ausgelassen: verschachteltes `Option`.
**Regeln**: Unfertiges machen wir ohne Rückfrage weiter; „fertig“ schreiben wir erst nach der Prüfung auf echten Systemen; keine Geheimnisse (z. B. Passwörter); keine neuen REST-APIs; Node.js nur für vom Eigentümer erlaubte Tests.

### Français
**État (vérifié sur de vrais systèmes)** : nous développons toujours ensemble les quatre projets sœurs (open-cosmo, RPoem, Reaxt, RPoRea). La passerelle répartit une question sur plusieurs serveurs et assemble les réponses ; elle gère `@key`, `@requires`, `@provides`, `@skip`, `@include`, les fragments nommés et la relecture après écriture. 59 tests de la passerelle, 22 tests de Reaxt et un essai de bout en bout de 21 contrôles sur de vrais serveurs réussissent. La jonction avec des implémentations autres que Reaxt (Apollo, async-graphql en Rust) est confirmée. Reaxt v0.1.3 est publié (`f32`, `Box<T>`, tuples jusqu'à 6 éléments).
**Prévu (à faire)** : ① vérifier sur de vrais Mac et iPhone (nous n'avons pas d'appareils) ; ② exécutions planifiées dans le cloud (GitHub est connecté, mais il faut les créer dans une nouvelle session ou sur le web ; pour pousser, il faut aussi installer la Claude GitHub App) ; ③ confirmer que la planification (cron) de `docs-latest` se déclenche vraiment. Exclu volontairement : `Option` imbriqué.
**Règles** : ce qui n'est pas fini continue sans demander ; nous n'écrivons « terminé » qu'après vérification sur de vrais systèmes ; pas de secrets (mots de passe…) ; pas de nouvelle API REST ; Node.js seulement pour les essais autorisés par le propriétaire.

### Español
**Estado (comprobado en sistemas reales)**: los cuatro proyectos hermanos (open-cosmo, RPoem, Reaxt, RPoRea) se desarrollan siempre juntos. La pasarela reparte una pregunta entre varios servidores y junta las respuestas; admite `@key`, `@requires`, `@provides`, `@skip`, `@include`, fragmentos con nombre y volver a leer tras escribir. Pasan 59 pruebas de la pasarela, 22 de Reaxt y una prueba de extremo a extremo de 21 comprobaciones con servidores reales. Se confirmó la unión con implementaciones distintas de Reaxt (Apollo y async-graphql de Rust). Reaxt v0.1.3 está publicado (`f32`, `Box<T>`, tuplas de hasta 6 elementos).
**Previsto (pendiente)**: ① comprobar en Mac y iPhone reales (no tenemos dispositivos); ② ejecuciones programadas en la nube (GitHub está conectado, pero hay que crearlas en una sesión nueva o en la web; para hacer push también hace falta instalar la Claude GitHub App); ③ confirmar que la programación (cron) de `docs-latest` realmente se dispara. Excluido a propósito: `Option` anidado.
**Reglas**: lo que falta continúa sin pedir confirmación; solo escribimos «terminado» tras verificar en sistemas reales; sin secretos (contraseñas); sin nuevas APIs REST; Node.js solo para pruebas que el propietario permita.

### Português
**Situação (verificada em sistemas reais)**: os quatro projetos irmãos (open-cosmo, RPoem, Reaxt, RPoRea) são sempre desenvolvidos juntos. O gateway divide uma pergunta entre vários servidores e junta as respostas; aceita `@key`, `@requires`, `@provides`, `@skip`, `@include`, fragmentos nomeados e reler após uma escrita. Passam 59 testes do gateway, 22 do Reaxt e uma execução de ponta a ponta com 21 verificações em servidores reais. A junção com implementações que não são Reaxt (Apollo e o async-graphql do Rust) foi confirmada. O Reaxt v0.1.3 foi publicado (`f32`, `Box<T>`, tuplas de até 6 elementos).
**Planejado (pendente)**: ① verificar em Mac e iPhone reais (não temos dispositivos); ② execuções agendadas na nuvem (o GitHub está conectado, mas é preciso criá-las numa nova sessão ou na web; para fazer push também é preciso instalar o Claude GitHub App); ③ confirmar que o agendamento (cron) do `docs-latest` realmente dispara. Excluído de propósito: `Option` aninhado.
**Regras**: o que falta continua sem pedir confirmação; só escrevemos «concluído» após verificar em sistemas reais; sem segredos (senhas); sem novas APIs REST; Node.js apenas em testes permitidos pelo proprietário.

### Русский
**Состояние (проверено на реальных системах)**: четыре родственных проекта (open-cosmo, RPoem, Reaxt, RPoRea) мы всегда развиваем вместе. Шлюз делит один вопрос между несколькими серверами и собирает ответы; поддерживаются `@key`, `@requires`, `@provides`, `@skip`, `@include`, именованные фрагменты и повторное чтение после записи. Проходят 59 тестов шлюза, 22 теста Reaxt и сквозной прогон из 21 проверки на настоящих серверах. Подтверждено объединение с реализациями, не являющимися Reaxt (Apollo и async-graphql на Rust). Выпущен Reaxt v0.1.3 (`f32`, `Box<T>`, кортежи до 6 элементов).
**Планируется (не завершено)**: ① проверка на настоящих Mac и iPhone (устройств нет); ② запуск по расписанию в облаке (GitHub подключён, но задачу нужно создать в новой сессии или в вебе; для push также нужно установить Claude GitHub App); ③ подтверждение того, что расписание (cron) `docs-latest` действительно срабатывает. Намеренно исключено: вложенный `Option`.
**Правила**: незавершённое продолжаем без вопросов; «готово» пишем только после проверки на реальных системах; секреты (пароли) не пишем; новых REST-API не создаём; Node.js — только для тестов, разрешённых владельцем.


## 2026-11-20 الوضع الحالي والخطة (العربية) / Status in Arabic (ترجمة بالذكاء الاصطناعي، لم يراجعها ناطق أصلي بعد)

**الوضع الحالي (تم التحقق منه على أنظمة حقيقية):** نطوّر المشاريع الأربعة الشقيقة (open-cosmo وRPoem وReaxt وRPoRea) دائماً معاً. تقسّم البوابة سؤالاً واحداً على عدة خوادم ثم تدمج الإجابات، وتدعم `@key` و`@requires` و`@provides` و`@skip` و`@include` والأجزاء المسمّاة (fragments) وإعادة القراءة بعد الكتابة. نجح 59 اختباراً للبوابة و22 اختباراً لـ Reaxt، وكذلك اختبار شامل من 21 فحصاً على خوادم حقيقية. وتأكد الدمج مع تطبيقات غير Reaxt (Apollo وasync-graphql في Rust). صدر Reaxt v0.1.3 (يدعم `f32` و`Box<T>` والصفوف المرتبة (tuples) حتى 6 عناصر).

**المخطط له (لم يكتمل بعد):** (1) التحقق على أجهزة Mac وiPhone الحقيقية (لا نملك الأجهزة)؛ (2) التشغيل الدوري في السحابة: أُنشئ الروتين `4sisters-light-work-every-2h` (كل ساعتين)، ويبقى تثبيت Claude GitHub App للسماح بالدفع (push)؛ (3) التأكد من أن الجدولة (cron) الخاصة بـ `docs-latest` تعمل فعلاً (لم تظهر بعد عملية بحدث `schedule`). مستبعد عمداً: `Option` المتداخل.

**القواعد:** نواصل العمل غير المنجز دون طلب تأكيد؛ لا نكتب «تم» إلا بعد التحقق على أنظمة حقيقية؛ لا نكتب معلومات سرّية (مثل كلمات المرور)؛ لا نضيف واجهات REST جديدة؛ ولا نستخدم Node.js إلا في الاختبارات التي سمح بها المالك.


## ۲۰۲۶-۱۱-۲۰ وضعیت و برنامه (فارسی) / Status in Persian (ترجمهٔ هوش مصنوعی؛ هنوز توسط فارسی‌زبان بومی بررسی نشده است)

**وضعیت کنونی (روی سامانه‌های واقعی بررسی شده):** چهار پروژهٔ خواهر (open-cosmo، RPoem، Reaxt و RPoRea) را همیشه با هم توسعه می‌دهیم. دروازه یک پرسش را میان چند سرور پخش می‌کند و پاسخ‌ها را در یک پاسخ واحد ترکیب می‌کند؛ از `@key`، `@requires`، `@provides`، `@skip`، `@include`، فرگمنت‌های نام‌دار و خواندن دوبارهٔ مقدارها پس از نوشتن پشتیبانی می‌کند. ۵۹ آزمون دروازه، ۲۲ آزمون Reaxt و یک آزمون سرتاسری با ۲۱ بررسی روی سرورهای واقعی موفق بود. ترکیب با پیاده‌سازی‌هایی غیر از Reaxt (Apollo و async-graphql در Rust) نیز تأیید شد. Reaxt نسخهٔ v0.1.3 منتشر شد (با پشتیبانی از `f32`، `Box<T>` و چندتایی‌ها تا ۶ عنصر).

**برنامه (هنوز انجام نشده):** (۱) بررسی روی دستگاه‌های واقعی Mac و iPhone (دستگاه نداریم)؛ (۲) اجرای زمان‌بندی‌شده در ابر: روتین `4sisters-light-work-every-2h` (هر دو ساعت) ساخته شد و برای push باید Claude GitHub App نصب شود؛ (۳) اطمینان از اینکه زمان‌بندی (cron) برای `docs-latest` واقعاً اجرا می‌شود (هنوز اجرایی با رویداد `schedule` دیده نشده است). عمداً کنار گذاشته شد: `Option` تودرتو.

**قواعد:** کارهای نیمه‌تمام را بدون پرسیدن ادامه می‌دهیم؛ فقط پس از بررسی روی سامانهٔ واقعی می‌نویسیم «انجام شد»؛ اطلاعات محرمانه (مثل گذرواژه‌ها) را نمی‌نویسیم؛ REST تازه اضافه نمی‌کنیم؛ و Node.js فقط برای آزمون‌هایی که مالک اجازه داده به کار می‌رود.


## 2026-11-21 定期実行の確認結果 / Scheduled-run verification (12 languages; AI翻訳・ネイティブ確認前 / AI-translated, not native-checked)

### 日本語
`docs-latest` の定期実行(`schedule`)が UTC 21:00(日本時間 06:00)に成功しました。クラウドのルーチン(2時間ごと)も5回動き、push も通りました(Claude GitHub App が必要という見立ては外れ)。ただし、設定の時刻(日本時間 10:07・12:07・22:07)とは違う時刻に動いたので、次の実行(UTC 01:07)で再確認します。未了は Mac・iPhone の実機確認(実機なし)だけです。

### English
The scheduled run of `docs-latest` (`schedule` event) succeeded at 21:00 UTC (06:00 Japan time). The cloud routine (every two hours) also ran five times and its pushes went through, so the assumption that the Claude GitHub App was required was wrong. However, it fired at a different time from the configured slots (10:07, 12:07 and 22:07 Japan time), so we will check again at the next run (01:07 UTC). The only open item is the real-device check on Mac and iPhone (no devices).

### 简体中文
`docs-latest` 的定时运行(`schedule` 事件)在 UTC 21:00(日本时间 06:00)成功。云端例程(每两小时)也运行了五次,推送均已成功,因此"需要 Claude GitHub App"的判断是错误的。不过它触发的时间与设定的时间(日本时间 10:07、12:07、22:07)不同,所以将在下一次运行(UTC 01:07)再次确认。唯一未完成的是 Mac 与 iPhone 的真机确认(没有设备)。

### 繁體中文
`docs-latest` 的排程執行(`schedule` 事件)在 UTC 21:00(日本時間 06:00)成功。雲端例行程序(每兩小時)也執行了五次,推送皆已成功,因此「需要 Claude GitHub App」的判斷是錯的。不過它觸發的時間與設定的時間(日本時間 10:07、12:07、22:07)不同,所以會在下一次執行(UTC 01:07)再次確認。唯一未完成的是 Mac 與 iPhone 的實機確認(沒有裝置)。

### 한국어
`docs-latest`의 정기 실행(`schedule` 이벤트)이 UTC 21:00(일본 시간 06:00)에 성공했습니다. 클라우드 루틴(2시간마다)도 다섯 번 실행되었고 push도 통과해서, "Claude GitHub App이 필요하다"는 판단은 틀렸습니다. 다만 설정한 시각(일본 시간 10:07, 12:07, 22:07)과 다른 시각에 동작했으므로, 다음 실행(UTC 01:07)에서 다시 확인합니다. 남은 일은 Mac과 iPhone 실기기 확인(기기 없음)뿐입니다.

### Deutsch
Der geplante Lauf von `docs-latest` (Ereignis `schedule`) war um 21:00 UTC (06:00 japanischer Zeit) erfolgreich. Auch die Cloud-Routine (alle zwei Stunden) lief fünfmal, und ihre Pushes gingen durch; die Annahme, die Claude GitHub App sei nötig, war also falsch. Allerdings lief sie zu einer anderen Zeit als konfiguriert (10:07, 12:07 und 22:07 japanischer Zeit), deshalb prüfen wir beim nächsten Lauf (01:07 UTC) erneut. Offen bleibt nur die Prüfung auf echten Mac- und iPhone-Geräten (keine Geräte vorhanden).

### Français
L'exécution planifiée de `docs-latest` (événement `schedule`) a réussi à 21:00 UTC (06:00 heure du Japon). La routine cloud (toutes les deux heures) s'est aussi exécutée cinq fois et ses push sont passés ; l'hypothèse selon laquelle la Claude GitHub App était nécessaire était donc fausse. Toutefois, elle s'est déclenchée à une heure différente de celles configurées (10:07, 12:07 et 22:07 heure du Japon) ; nous vérifierons donc à la prochaine exécution (01:07 UTC). Il ne reste que la vérification sur de vrais Mac et iPhone (nous n'avons pas d'appareils).

### Español
La ejecución programada de `docs-latest` (evento `schedule`) tuvo éxito a las 21:00 UTC (06:00 hora de Japón). La rutina en la nube (cada dos horas) también se ejecutó cinco veces y sus push funcionaron, así que la suposición de que hacía falta la Claude GitHub App era errónea. Sin embargo, se disparó a una hora distinta de las configuradas (10:07, 12:07 y 22:07 hora de Japón), por lo que volveremos a comprobarlo en la próxima ejecución (01:07 UTC). Lo único pendiente es la comprobación en Mac y iPhone reales (no tenemos dispositivos).

### Português
A execução agendada do `docs-latest` (evento `schedule`) teve sucesso às 21:00 UTC (06:00 no horário do Japão). A rotina na nuvem (a cada duas horas) também rodou cinco vezes e seus push funcionaram, então a suposição de que o Claude GitHub App era necessário estava errada. Porém, ela disparou num horário diferente dos configurados (10:07, 12:07 e 22:07 no horário do Japão), por isso voltaremos a verificar na próxima execução (01:07 UTC). O único item pendente é a verificação em Mac e iPhone reais (não temos dispositivos).

### Русский
Запуск `docs-latest` по расписанию (событие `schedule`) успешно прошёл в 21:00 UTC (06:00 по японскому времени). Облачная рутина (каждые два часа) тоже выполнилась пять раз, и её push прошли, так что предположение о необходимости Claude GitHub App оказалось неверным. Однако она сработала не в настроенное время (10:07, 12:07 и 22:07 по японскому времени), поэтому мы проверим снова при следующем запуске (01:07 UTC). Не завершена только проверка на настоящих Mac и iPhone (устройств нет).

### العربية
نجح التشغيل المجدول لـ `docs-latest` (حدث `schedule`) في الساعة 21:00 بالتوقيت العالمي (06:00 بتوقيت اليابان). كما عمل الروتين السحابي (كل ساعتين) خمس مرات ونجحت عمليات الدفع (push)، فكان افتراض الحاجة إلى Claude GitHub App خاطئاً. غير أنه عمل في وقت مختلف عن الأوقات المضبوطة (10:07 و12:07 و22:07 بتوقيت اليابان)، لذا سنعيد التحقق في التشغيل التالي (01:07 بالتوقيت العالمي). والبند الوحيد المتبقي هو التحقق على أجهزة Mac وiPhone الحقيقية (لا نملك الأجهزة).

### فارسی
اجرای زمان‌بندی‌شدهٔ `docs-latest` (رویداد `schedule`) ساعت ۲۱:۰۰ به وقت UTC (۰۶:۰۰ به وقت ژاپن) موفق بود. روتین ابری (هر دو ساعت) هم پنج بار اجرا شد و push‌ها انجام شد؛ پس فرض لازم‌بودن Claude GitHub App نادرست بود. با این حال در زمانی متفاوت از زمان‌های تنظیم‌شده (۱۰:۰۷، ۱۲:۰۷ و ۲۲:۰۷ به وقت ژاپن) اجرا شد، بنابراین در اجرای بعدی (۰۱:۰۷ UTC) دوباره بررسی می‌کنیم. تنها کار باقی‌مانده بررسی روی دستگاه‌های واقعی Mac و iPhone است (دستگاه نداریم).
