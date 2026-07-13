# 技術スタック・開発ルール(open-runo)

**このリポジトリは廃止されていません。** 2026-07-10にユーザー指示により、
[`poem-cosmo-tauri`](https://github.com/aon-co-jp/poem-cosmo-tauri) と
**同時並行で開発**する方針に変更されました。両リポジトリとも
**Tauri・Poem を含めない**構成で進めます(open-runoはさらに厳密に
Tauri/Poemを一切含まない方針。poem-cosmo-tauri側は互換性維持のため
名称にPoem/Tauriを残しつつ実体はTauri/Poem非依存)。共通点: どちらも
WunderGraph Cosmo(有料版含む)をパッケージとして直接依存させず、
Rust標準ライブラリ + tokio/hyperで機能を自前実装する。
実装作業(例: crates/open-runo-routerのPoem→tokio/hyper移行)は
poem-cosmo-tauri側で先行し、動作確認が取れたファイルをこちらにも
ミラーしていく運用とする。

## poem-cosmo-tauri と open-runo の違い(2026-07-11、ユーザー確認済み)

両リポジトリは共通コアを持つが、**スコープが異なる別々のリポジトリ
プロジェクト**であり、統合・一本化すべき対象ではない。

- **共通コア**: WunderGraph Cosmo 有料版の機能(GraphQL Federation・
  VersionlessAPI・SSO/SCIM/RBAC・Persisted Queries・キャッシュ制御・
  細粒度レートリミット等)を、Cosmo自体には依存せず Rust + tokio/hyper で
  自前再実装した OSS 版。これは両リポジトリで共通。
- **poem-cosmo-tauri はさらに範囲が広い**: 共通コアに加えて、Poem(Rust
  Web フレームワーク)と Tauri(デスクトップフロントエンドフレームワーク)
  の**全機能を、AI駆動開発によって一から自作・再現する**ことを目指す
  ——単にAPI形状・体験の互換性を保つだけでなく、両フレームワークの
  機能そのものを自前実装として再現する、という上乗せの目標を持つ。
  **このリポジトリ(open-runo)にはこの上乗せ目標はない**——Cosmo
  パリティの共通コアが中心。
- 両リポジトリは共通コアを持つが**全く違うリポジトリのプロジェクト**であり、
  「ミラー」作業は必ずしも「同一スコープの複製」を意味しない——
  poem-cosmo-tauri 固有の Poem/Tauri 機能再現タスクがこちらに存在理由なく
  持ち込まれることもあれば、逆にこちらが独自に先行実装し
  poem-cosmo-tauri へ逆ミラーするケースもある(例:
  `open-runo-feature-flags`、2026-07-11)。

## poem-cosmo-tauri の構成・位置付け(2026-07-11、ユーザーによる最終定義)

poem-cosmo-tauri は、以下の3要素をすべて**外部パッケージに依存せず自前で
一から開発・再現**し、それらの連携をスムーズに行うことで、WEBサイト/
WEBアプリ開発を効率的に行えるようにするための**フレームワーク/ミドル
ウェア**である。3要素いずれも「連携」ではなく、そのフレームワーク自体の
完全互換な自前再実装を指す点に注意(2026-07-11、ユーザーによる訂正)。

1. **cosmo部分(= このリポジトリ open-runo と共通のコア)**: WunderGraph
   Cosmo 有料版(Launch/Scale/Enterprise)の機能を、Cosmo自体には依存
   せず Rust + tokio/hyper で自前再実装した OSS 版。具体的には (a)
   Tauri互換のフロントエンド体験、(b) **REST API不要**(VersionlessAPI/
   GraphQL Federationで代替しエンドポイントのバージョン乱立を根本解決)、
   (c) **契約不要**(Cosmo有料版であれば必要な商用ライセンス契約なしに
   同等機能をOSSとして提供)、(d) **独自AI搭載のWeb高速化機能**
   (自己学習型HTMLキャッシュ予測=`CachePredictor`によるコールドスタート
   予測・コスト学習・適応TTL等、外部LLM/有料契約は一切不要な純Rust
   統計学習)を含む。**このリポジトリ(open-runo)はこのcosmo部分が中心**。
2. **poem部分(= バックエンド、poem-cosmo-tauri固有)**: Rust の Poem
   フレームワークの**全機能を完全互換で一から自作・再現**したバック
   エンド(`poem`パッケージへの直接依存は持たない)。
3. **tauri部分(= フロントエンド、poem-cosmo-tauri固有)**: デスクトップ
   フロントエンドフレームワーク Tauri の**全機能を完全互換で一から自作・
   再現**したフロントエンド(`tauri`パッケージへの直接依存は持たない)。

**この3つ(Tauri再現フロントエンド + open-runo/cosmoコア + Poem再現
バックエンド)がスムーズに連携し合うこと自体が poem-cosmo-tauri の価値**
であり、**このリポジトリ(open-runo)にはpoem/tauri部分の統合という上乗せ
目標はない**——cosmo部分(共通コア)の完成度・利便性・使いやすさ・実用性
向上が中心。新機能・改善タスクを検討する際は上記4特性を軸とする。

このリポジトリ、および関連プロジェクト(`open-web-server`/`aruaru-db`/
`aruaru-web`/`open-raid-z`)で開発・保守を行う際は、以下を基本方針とする。
作業ドライブは `F:\open-runo`(E:ドライブは2026-07-10に消失、以後Fが実体)。
この節は [`open-raid-z`](https://github.com/aon-co-jp/open-raid-z) の
`CLAUDE.md` を正本とし、各プロジェクトへコピーして同期する。

## open-web-server 拡張要件との関わり(2026-07-13、要約を統合・整理)

`open-web-server` は、3Dオンラインゲームのアイテム課金やクレジット
カード決済のような金融データを扱う、24時間365日ノンストップ運用の
ミッションクリティカルな Web サーバー。4層防御通信による高セキュリティ
と高速性の両立、およびZFS互換(`open-raid-z`)とACID互換(PostgreSQL)の
ハイブリッド技術を核として、poem-cosmo-tauri(またはこのリポジトリ)・
PostgreSQL・`aruaru-db`・`open-raid-z`と連携する多層防御アーキテクチャ
により、二重課金・データ消失を防ぐ。通信層の四重化(TCP-IP・UDP-IP・
QUIC・MPTCP/SCTP相当)・DB書き込みの四重化(PostgreSQL・aruaru-db・
マルチリージョン同期レプリケーション・独立監査ログ、全系統実装済み)・
VersionLessAPIとGit管理のハイブリッド版管理の詳細・進捗は
`open-web-server/CLAUDE.md`(および正本の open-raid-z `CLAUDE.md`)を
参照。このリポジトリはその Federation Gateway/バックエンド側として
関与しうる。

## フロントエンド(2026-07-10、方針更新)

- Tauriパッケージには直接依存しない。ただしTauriのデスクトップUI体験・
  `invoke()`的なコマンド呼び出しインターフェースとは互換性を保つ。
- **HTML5/CSS3・TypeScript・Bootstrap・Node.jsのスタックは廃止**。
  Rustをメイン言語としてフロントエンドとバックエンドを統合し、
  **WebAssembly (WASM)** に置き換える(コンパイル対象はRust →
  `wasm32-unknown-unknown`)。DOM操作・`invoke()`相当の呼び出しは
  Rust製WASMモジュール側で行い、TypeScript/Node.jsのビルドチェーンには
  依存しない。https://webassembly.org/ | https://rustwasm.github.io/
- Tauri機能パリティ調査(参照: https://v2.tauri.app/ |
  https://github.com/tauri-apps/tauri)の結果は`docs/tauri-parity.md`
  を正とする。

## バックエンド・コア

- **Rust**(メイン言語、標準ライブラリ中心): https://www.rust-lang.org/ja/ | https://github.com/rust-lang/rust
- **tokio** + **hyper**(Webフレームワークなしで直接HTTPサーバを自前実装):
  https://tokio.rs/ | https://docs.rs/hyper/latest/hyper/
- Poemパッケージには依存しないが、Poemのルーティング/ハンドラAPI形状とは
  互換性のあるインターフェースを維持しながらtokio/hyper直接実装へ移行する
  (既存ハンドラのシグネチャ・レスポンス形式を極力変えない。移行状況は
  下記HANDOFF参照)。参考資料: https://docs.rs/poem/latest/poem/ |
  https://github.com/poem-web/poem |
  https://zenn.dev/ouvill/articles/introduce_rust_poem_framework。
  機能差分の調査結果は`docs/poem-parity.md`を正とする。

## API設計思想(参考・概念のみ)

- **VersionLess API**という考え方を参考にする(WunderGraphのブログ/podcast参照)。
- **WunderGraph Cosmo**: あくまで**参考・着想元としてのみ**参照する。
  **有料版を含め実装には絶対に使用しない**。https://github.com/wundergraph/cosmo
- 「REST APIを不要にする」仕様はCosmo有料版と同一方針であることを2026-07-11に
  確認済み。機能対応表・未実装ギャップは`docs/cosmo-parity.md`(4a節)を正とする。
- **「APIキー不要」の正確な意味(2026-07-11、ユーザー確認済み)**: 認証を
  廃止するのではなく、**人間がAPIキーを意識・管理する必要をゼロにする**
  という意味。`KeyGuardian`のauto-issue/auto-revoke/auto-clean/
  auto-defendに加え、`POST /api/keys/self-issue`(認証不要でdeveloper
  role・24時間有効のキーを自動発行)とWASMフロントエンドの透過的な
  取得・キャッシュ・401時自動再発行で実現済み。

## 関連プロジェクト

- **poem-cosmo-tauri**(姉妹リポジトリ・同時並行開発。GraphQL Federation /
  API Gateway / AI-native routing platform。実装の先行地点):
  https://github.com/aon-co-jp/poem-cosmo-tauri
- **open-runo**(このリポジトリ。2026-07-10付けで開発再開・poem-cosmo-tauri
  と同時並行で開発): https://github.com/aon-co-jp/open-runo
- **open-web-server**: https://github.com/aon-co-jp/open-web-server
- **aruaru-db**: https://github.com/aon-co-jp/aruaru-db
- **aruaru-web**: https://github.com/aon-co-jp/aruaru-web
- **open-raid-z**(開発ルールの正本): https://github.com/aon-co-jp/open-raid-z
- **rs-to-readme**: https://github.com/aon-co-jp/rs-to-readme

## 運用ルール

- **開発中はこの`CLAUDE.md`を、コード変更のコミット/pushと必ず一緒に
  push する**(内容を更新した場合はもちろん、変更が無い場合も他の変更と
  一緒にコミット対象へ含めておくこと)。
- 実装で迷った場合や、API仕様の詳細確認が必要な場合は、学習データからの
  推測より公式ドキュメント(上記URL)を優先して参照する。
- 作業ドライブが変わった場合は、この節を更新し、関連プロジェクトの
  引き継ぎ資料にも変更の経緯を記録すること。
- **各無人開発パスの最後には、必ず以下を実行すること**(ユーザー指示、
  2026-07-11): (1) 世界10ヶ国語のREADME(`README-<言語>.md`、日本語・
  英語・中国語簡体字・韓国語・スペイン語・フランス語・ドイツ語・
  イタリア語・ロシア語・アラビア語)を、そのパスで行った実装内容に
  合わせて更新する、(2) 他プロジェクトへのお引越し可能ファイル
  (`PORTING.md`)を同様に最新の実装内容に合わせて更新する、(3) この
  開発環境ルール(`CLAUDE.md`)のHANDOFF節・現状節を更新して保存する、
  (4) 上記(1)〜(3)を含む全ての変更をコミットしてpushする。この4点を
  終えてから、そのパスを終了する。
- **ドキュメント(parity調査・ギャップ一覧・HANDOFF等)に「未調査」「未着手」
  「意図的に先送り」「将来検討」等と記録されている項目を見つけた場合、
  それは承認待ちの提案ではなく、次に着手すべき実装対象そのものである**
  (ユーザー指示、2026-07-12)。「ご指示はありますか?」のように追加の
  確認を求めて手を止めず、確認不要でそのまま実装を進めること。
  優先度・実現可能性の判断(例: ブラウザ実行という設計上の制約で
  技術的に不可能な項目は代替案を検討する)は自分で行ってよいが、
  「未着手だから今回は見送る」という判断そのものをユーザーへの
  確認なしに下してはならない——見送る場合も、まず着手を試み、
  真に不可能/著しく非現実的と判明した場合のみ、その理由をドキュメントに
  明記した上で次の項目に進む。
- **技術選定・仕様確認で迷った場合、必要に応じて日本語と英語の両方で
  Google検索し、Claude(自分自身)の知識・推論も動員し、GitHubでも
  調査すること**(ユーザー指示、2026-07-13)。
  学習データからの推測だけに頼らず、実在するクレート・ライブラリの
  現状(バージョン・メンテナンス状況・プラットフォーム対応)や、
  最新の実務知見(2026年時点のベストプラクティス等)を実際に検索して
  裏付けを取ってから実装判断を下す。日本語のみ・英語のみでは見つからない
  情報が言語を変えると見つかることがあるため、両言語での検索を基本とする。
- **よほど確認が必要な場面(重大な破壊的操作・仕様の根本方針転換等)を
  除き、確認を求めて手を止めないこと**(ユーザー指示、2026-07-13)。
  技術選定や実装方法で分からないこと・迷うことがあれば、まず上記の通り
  日本語・英語両方でのGoogle検索・GitHub調査を行い、それでも判断が
  つかない場合は自分の工学的判断で最も妥当な選択をして実装を進める。
  「〜については確認が必要です」と言って作業を止め、ユーザーの回答を
  待つことを既定の振る舞いにしない。

## 現状(このリポジトリ固有)

- `cargo check --workspace` / `cargo test --workspace` は成功する
  (18クレート構成。2026-07-13時点で`open-runo-router`単体146テスト・
  `open-runo-observability`9テスト含め全体failed 0)。
  todo!()/unimplemented!()マーカーなし。
- 直近パスで追加された機能(poem-cosmo-tauriからミラー): 月間リクエスト数
  計測+Analytics(`open-runo-observability::request_metrics`、
  `GET /api/analytics/requests-per-month` `/operations`、
  `apps/desktop-wasm`のAnalyticsページ、EDFSモジュール`edfs.rs`も
  このパスで初めてこちらへ移植——poem-cosmo-tauri側で先行実装済み
  だったが未ミラーだったことをlib.rsのモジュール差分で発見)。
  それ以前: Feature
  Flags REST API + WASM管理画面(`open-runo-feature-flags`)、
  gzipレスポンス圧縮ミドルウェア、汎用WebSocket対応(手書きRFC 6455、
  `GET /api/ws-echo` / `GET /api/ws-events`)、Federation v1/v2
  SDLパーサー(`open-runo-federation::sdl`、`POST
  /api/federation/compose`の`sdl`フィールド)、DB REST型集約
  (`open-runo-api-types`への統合、`table`フィールド欠落バグ修正)、
  `open-runo-cli`の`db`サブコマンド。
- README多言語版は `README-<言語>.md` 形式で日本語・英語・中国語簡体字・
  韓国語・スペイン語・フランス語・ドイツ語・イタリア語・ロシア語・
  アラビア語の10言語が揃っている。

## HANDOFF(直近の自動実行パス)

- **2026-07-13 月間リクエスト数計測 + Analytics(Cosmo Studio相当)を
  poem-cosmo-tauriからミラー完了(docs/cosmo-parity.md 4a節の残り2件を
  両方解消) — EDFSモジュール未ミラーの発見・是正も含む**:
  poem-cosmo-tauri側コミット`2bb5363`("Add monthly request-count metering
  + Cosmo Studio-style Analytics dashboard")で実装・実バイナリ+実ブラウザ
  検証済みだった`crates/open-runo-observability/src/request_metrics.rs`
  (`RequestMetrics`: 月別カウント+method/pathごとのcount/error_count/
  total_duration_ms集計、`MetricsSink` trait経由でバッファをClickHouseへ
  非同期flush)・`middleware_hyper::with_metrics`・`AppState.request_metrics`
  ・`GET /api/analytics/requests-per-month` `/operations`・
  `apps/desktop-wasm`のAnalyticsページ(計10ページ)をそのままコピーして
  ミラー。**ミラー中に発見した既存の未解消drift**: `lib.rs`を丸ごと
  コピーしたところ`pub mod edfs;`の参照先(`edfs.rs`)がこのリポジトリに
  一度も移植されていなかったことが`cargo check`のコンパイルエラー
  (`E0583 file not found for module`)で判明——EDFS(Event-Driven
  Federated Subscriptions、`docs/cosmo-parity.md`4a節で
  poem-cosmo-tauri側は2026-07-12に完了済みだったが、こちらへの
  ミラーが漏れていた)。`crates/open-runo-router/src/edfs.rs`を追加で
  移植して解消(このパスの主目的ではないが、放置すると今後の丸ごと
  コピー型ミラーが毎回同じ理由で壊れるため合わせて対応)。
  **検証**: `cargo check --workspace`green(既存3警告のみ)、
  `cargo test --workspace`(open-runo-router: 146テスト、
  open-runo-observability: 9テスト)ともfailed 0。実バイナリ+curlで
  このリポジトリ自身に対しても`OPEN_RUNO_BIND_ADDR=127.0.0.1:18944`で
  独立に自己発行キー取得→`/api/analytics/requests-per-month`
  `/api/analytics/operations`が実データを返すことを再確認(poem-cosmo-tauri
  側と同じ結果)。**未検証点(poem-cosmo-tauri側と同一の理由で継続)**:
  実ClickHouseインスタンスがこのサンドボックスに無いため、
  `ClickHouseSink`の実ラウンドトリップは`#[ignore]`テストのまま
  (`OPEN_RUNO_CLICKHOUSE_URL`環境変数で実インスタンス相手に明示実行可能)。
  `docs/cosmo-parity.md`4a節の該当2行を取り消し線+「✅ 完了」に更新
  (詳細な実装記録はpoem-cosmo-tauri側の同日CLAUDE.md HANDOFFエントリを
  正とする)。
  次回パスがすべきこと: (1) 実ClickHouseインスタンスが用意でき次第、
  両リポジトリの`#[ignore]`テストを実行して実ラウンドトリップを確認、
  (2) `docs/cosmo-parity.md`4a節はこれで全項目✅完了(旧★★☆が0件に)
  ——次に高価値なタスクを探す場合は`docs/poem-parity.md`/
  `docs/tauri-parity.md`の残ギャップ、または今後poem-cosmo-tauri側で
  新規実装される機能のミラー待ちを継続、(3) 丸ごとファイルコピーで
  ミラーする際は`cargo check --workspace`を必ず先に通し、今回のような
  「モジュール宣言はあるがファイルが無い」drift(過去にpoem-cosmo-tauri
  側が先行実装した機能の未ミラー)を早期発見すること。

- **2026-07-13 OpenAPI spec coverage拡大をpoem-cosmo-tauriからミラー
  完了(docs/api-examples.md Coverage note指摘の実ギャップ解消)**:
  poem-cosmo-tauri側で先行実装・検証済みの`crates/open-runo-router/
  src/openapi.rs`変更(DB CRUD 8型・Feature Flags 4型を
  `components.schemas`に追加、`/api/db/*`各パスを型付きレスポンス/
  リクエストボディに書き換え、丸ごと欠落していた`/api/feature-flags`・
  `/api/feature-flags/:name`・`/api/feature-flags/:name/evaluate`の
  3パスをspecに新規追加)+新規テスト
  `db_and_feature_flag_endpoints_are_typed_and_feature_flags_are_documented`
  をこちらへコピーしてミラー(`crates/open-runo-api-types/src/lib.rs`・
  `crates/open-runo-router/src/lib.rs`は既に同一内容だったため無変更)。
  `docs/cosmo-parity.md`のMCP Server行(古い「未実装」記述のまま残って
  いた実際は2026-07-12完了済み)・`docs/api-examples.md`のCoverage note
  も同期。**検証**: `cargo check --workspace`警告のみ(既存3件)で成功、
  `cargo test --workspace`全スイートgreen(失敗ゼロ、新規2テスト含む)。
  次回パスがすべきこと: `docs/cosmo-parity.md`4a節の残りギャップ
  (EDFS/Kafka連携・gRPC Connect対応、いずれも★★☆)、または残る
  未型付けエンドポイント(SCIM・persisted queries・cache/backup・
  migrate・integrity、約20本)への同様の型付け拡張。

- **2026-07-12 `docs/poem-parity.md`4a節の残りギャップ(ACME・gRPC・
  MCP Server)をpoem-cosmo-tauriからミラー完了——これで同ドキュメントの
  未実装項目はゼロに**: poem-cosmo-tauri側コミット`a10a4cf`(ACME、
  RFC 8555 HTTP-01のみ、`ring`のES256 JWS署名+モックCAサーバーとの実
  ラウンドトリップテスト)・`a5d3643`(gRPC、`grpc.health.v1.Health/Check`、
  新規依存無し、`OPEN_RUNO_GRPC_BIND_ADDR`で有効化)・`8e9ef94`(MCP
  Server、`POST /mcp`、JSON-RPC 2.0、`health_check`/`self_issue_api_key`
  の2実ツール)をこちらへ同期(`e66df73`〜`eb08c98`)。実装詳細・実機検証の
  記録はpoem-cosmo-tauri側CLAUDE.mdの同日エントリが正——このリポジトリ
  側でも`cargo test --workspace`(302テスト)/`--all-features`(311
  テスト、tls/acme feature込み)がgreenであることを個別に確認済み。
  次回パスがすべきこと: 各機能の対応範囲拡大(gRPCの他サービス・
  ストリーミング、MCPのResources/Prompts、DNS-01/TLS-ALPN-01チャレンジ、
  Cookie/セッション認証の他ハンドラへの段階的拡大)。急ぎではない。

- **2026-07-12 Poem/Tauriパリティの残ギャップをpoem-cosmo-tauriからミラー
  完了(Multipart・Cookie/セッション+CSRF・TLS・ネイティブ通知・
  システムトレイ・ネイティブインストーラー)**: poem-cosmo-tauri側コミット
  `401e9fe`(Multipart)・`2a999fb`(Cookie/セッション+CSRF)・`91c1c36`(TLS)・
  `a5ad6ca`(Web Notifications)・`8392c8f`(apps/desktop-tray新設)を
  こちらへ同期(`c72cf01`〜`0dffe6e`)。実装詳細・実機検証の記録は
  poem-cosmo-tauri側CLAUDE.mdの同日エントリが正——このリポジトリ側でも
  `cargo test --workspace`(286テスト)/`--all-features`(289テスト、tls
  feature込み)がgreenであること、`apps/desktop-tray`が
  `cargo build --release`で単独ビルドできることを個別に確認済み。
  ユーザー指示により「未着手・意図的に先送り」は確認を求めず実装対象と
  する運用ルールを明文化(全関連リポジトリのCLAUDE.mdに転記済み)。
  次回パスがすべきこと: ACMEクライアント・gRPC・MCP Server(poem-parity
  4a節参照)、Cookie/セッション認証の他ハンドラへの段階的拡大、
  `apps/desktop-tray`のmacOS/Linuxパッケージング。

- **2026-07-11 Federation v1/v2互換ギャップ解消をpoem-cosmo-tauriから
  ミラー完了(docs/cosmo-parity.md 4a節、★☆☆)**: poem-cosmo-tauri側
  コミット`b65013e`で実装・実バイナリ検証済みだった、SDLパーサー新設
  (`crates/open-runo-federation/src/sdl.rs`: `parse_service_sdl`/
  `detect_federation_version`)+`POST /api/federation/compose`への
  `sdl: Option<String>`配線(`crates/open-runo-router/src/
  handlers_hyper.rs`のServiceInput)を、同じ`lib.rs`/`sdl.rs`/
  `handlers_hyper.rs`/`docs/cosmo-parity.md`/`docs/federation.md`をこちら
  へコピーする形でミラー。`cargo test --workspace`(全37テストバイナリ、
  open-runo-federation: 4→11テスト)でfailed 0を確認後、
  **このリポジトリ自身の実バイナリで再検証**(`cargo run -p
  open-runo-router`、`OPEN_RUNO_BIND_ADDR=127.0.0.1:18722`):
  本物のFederation v1スタイル部分グラフ(bare `@key`/`@external`、
  `@link`無し)と本物のv2スタイル部分グラフ(`@link(url:
  "https://specs.apollo.dev/federation/v2.3"...)`+`@shareable`)を
  同一の`POST /api/federation/compose`リクエストで送信し、
  poem-cosmo-tauri側と**バイト単位で同一のレスポンス**
  (`{"contributing_services":["users-service-v1","billing-service-v2"],
  "types":{"Query":["billingHealth","me"],"Review":["author","body","id"],
  "User":["balanceCents","id","name","plan","reviews"]},
  "breaking_changes":[]}`、`GET /api/federation/status`も
  `type_count:3, field_count:10`一致)を確認済み——ファイルコピーだけで
  終わらせず、実際にcommit・push完了したことを本エントリと
  `git log`で確認できる状態にしている(直後の`git log`確認を参照)。
  次回パスがすべきこと: `docs/cosmo-parity.md`4a節の残りのギャップ
  (EDFS/Kafka連携・gRPC Connect対応・MCP Server統合)から次を選ぶ。

- **2026-07-11 汎用WebSocket対応をpoem-cosmo-tauriからミラー完了
  (docs/poem-parity.md 3節、★★☆ギャップを解消)**: poem-cosmo-tauri側
  コミット`53b10bf`で実装・実バイナリ検証済みだった、外部WebSocket
  フレームワークを使わない**手書きRFC 6455実装**
  (`crates/open-runo-router/src/hyper_compat.rs`の
  `websocket_handler`/`WebSocketConnection`/フレームのパース・生成・
  base64エンコード、いずれも手書き。唯一の追加依存は`sha1`
  ——`Sec-WebSocket-Accept`のSHA-1計算のみに使用)を、
  `hyper_compat.rs`・`handlers_hyper.rs`(`ws_echo_handler`/
  `ws_events_handler`+テスト2本)・`lib.rs`(`GET /api/ws-echo`/
  `GET /api/ws-events`の配線)・ルート`Cargo.toml`(`sha1`追加、
  既存の`toml = "0.8"`行はFederatedBackend用途でこのリポジトリ固有の
  ため保持したまま)・`open-runo-router/Cargo.toml`(`sha1`+
  テスト専用dev-dependency`tokio-tungstenite`)・`docs/poem-parity.md`
  をそのままコピーしてミラー。`hyper_compat::serve`の
  `http1::Builder`に`.with_upgrades()`を追加した変更点(これが
  ないと`hyper::upgrade::on`が解決せずWSハンドラがハングするだけの
  実バグになる、poem-cosmo-tauri側で実装中に発見済み)も含む。
  `cargo check --workspace` / `cargo test --workspace`
  (open-runo-router: 94テスト、`websocket_echo_round_trip_over_real_tcp`・
  `ws_events_rejects_missing_api_key`含む)ともfailed 0を確認。
  **実バイナリでも独立に再検証**: `OPEN_RUNO_BIND_ADDR=
  127.0.0.1:18412`で`cargo run -p open-runo-router`を起動し、
  Node.js 26組み込みの`WebSocket`クライアントから`ws://127.0.0.1:18412/
  api/ws-echo`に接続→`open-runo echo check`を送信→同一文字列がそのまま
  エコーされて返ってくることを確認→クリーンにclose(poem-cosmo-tauri側
  で確認した動作と同一結果)。
  `docs/poem-parity.md`2節・3節・4節のWebSocket関連行を取り消し線+
  「✅ 完了」に更新(内容はpoem-cosmo-tauri側と一致させたが、CRLF/LF
  差異はそのまま維持——このリポジトリの当該ファイルは元々CRLFだった
  ため、Editツールでの部分編集にとどめ全文上書きはしていない)。
  `git status`clean化後commit・push(コミットハッシュは`git log`で
  確認したものを次回パスへの引き継ぎとして記載する——ファイルコピー
  だけで「ミラー完了」と書かないという前々回パスの教訓を踏襲)。
  次回パスがすべきこと: `docs/cosmo-parity.md`4a節の残りのギャップ
  (EDFS/Kafka連携・gRPC Connect対応・MCP Server統合、いずれも
  ★★☆以下・実装コスト大)から次の実用性向上タスクを選び、
  poem-cosmo-tauriで先行実装した上でこちらへミラーを継続(ユーザー
  指示により確認不要で自動継続)。

- **2026-07-11 gzip応答圧縮ミドルウェア実装をopen-runoへミラー完了
  (docs/poem-parity.md 3節、★★☆ギャップを解消 — 実装自体は
  poem-cosmo-tauri側で既に完了・push済みだったコミット
  `9a2e209`だったが、このリポジトリへのミラーが未完了のまま残っていた)**:
  セッション開始時、このリポジトリには`middleware_hyper.rs`/
  `hyper_compat.rs`/`lib.rs`/`Cargo.toml`/`docs/poem-parity.md`に
  未コミットの差分(=前パスが部分的にミラー作業をしていた形跡)が
  既に存在していた。中身を`poem-cosmo-tauri`側の該当コミットと
  diffで突き合わせたところ、`with_compression`本体・テスト3本
  (`compression_gzips_large_body_when_accepted`/
  `compression_is_skipped_without_accept_encoding`/
  `compression_skips_small_bodies_even_when_accepted`)・
  `build_hyper_app`の配線(`with_compression`を最も外側にラップ —
  rate-limit超過やCORS preflightのレスポンスも含めて圧縮対象にするため)
  ・`docs/poem-parity.md`の更新は**すべて既にpoem-cosmo-tauriと一字一句
  一致した状態でこのリポジトリにコピー済み**であることを確認(差分ゼロ)。
  つまり実質的な実装作業は前パスで終わっており、本パスは
  **検証とcommit/pushだけが未完了だった**状態。
  `cargo check --workspace`・`cargo test --workspace`
  (全テストバイナリ、open-runo-router: 92テスト中
  `compression_*`3本含む)がgreenであることを実際に確認。
  **実バイナリ+curlでも独立に再検証**: `OPEN_RUNO_BIND_ADDR=
  127.0.0.1:18322`で`cargo run -p open-runo-router`を起動し、
  `GET /api/openapi.json`を(1)`Accept-Encoding`無しで叩くと
  `content-length: 10265`の生JSON、(2)`Accept-Encoding: gzip`付きで
  叩くと`content-encoding: gzip`+`content-length: 2115`(約79%削減、
  poem-cosmo-tauri側で測定された数値と完全一致)、(3)`curl --compressed`
  で自動デコードした結果が(1)のバイト列と`diff`で完全一致することを
  確認(poem-cosmo-tauri側でも同一の3ステップを再現し同じ結果を得た)。
  `git status`はclean化のうえcommit・push(コミットハッシュは次回HANDOFF
  更新時に追記)。brotliは意図的に見送り(pure-Rustの低リスクな
  brotliエンコーダcrateが無いため、gzip-onlyが今回の実用的な第一歩という
  判断、`docs/poem-parity.md`3節に記載済み)。
  次回パスがすべきこと: (1) `docs/cosmo-parity.md`4a節・
  `docs/poem-parity.md`3節の残りのギャップ(汎用WebSocket対応・
  EDFS/Kafka連携・gRPC Connect対応・MCP Server統合)から次の実用性向上
  タスクを選ぶ、(2) brotli対応が必要になった場合はpure-Rustの低リスクな
  エンコーダcrateが登場していないか確認、(3) 全体`cargo check
  --workspace` / `cargo test --workspace`を定期的に確認しつつ両
  リポジトリへのミラー・pushを継続(ユーザー指示により確認不要で
  自動継続)。

- **2026-07-11 未検証だった作業途中コードの検証・完成 + Feature Flags
  REST API実装(docs/cosmo-parity.md 4a、★★☆ギャップを解消) —
  両リポジトリ4コミット**: セッション開始時点で両リポジトリに未コミット
  の作業途中コードがあった。
  **(1) poem-cosmo-tauri側**(先に対応): DB REST型集約
  (`DbRecordListResponse`等を`open-runo-api-types`へ集約、フロントエンド
  の旧コピーが`table`フィールドを欠落させていたバグの修正)が未検証の
  まま残っており、`cargo check --workspace`で検証したところ
  `handlers_hyper.rs`に`DeleteResponse`(リネーム漏れ)への参照が1箇所
  残っておりコンパイル不能という実バグを発見・修正。あわせて
  `open-runo-cli`の未使用`put`/`delete`ヘルパーを使う`db`サブコマンドを
  新規実装。`cargo test --workspace`green確認後commit・push
  (`7aecb52`)。
  **(2) このリポジトリ側**: 本セッションの主作業として、既に
  `AppState`へ配線済みだった`open-runo-feature-flags`crate
  (`FeatureFlagRegistry`: upsert/get/list/delete/evaluate、
  `DefaultHasher`による決定的0-100バケッティング)に**REST APIハンドラを
  新規実装**(既存ハンドラが1本も無い状態だった)。`POST/GET
  /api/feature-flags`(upsert/list)・`GET/DELETE
  /api/feature-flags/:name`(get/delete、404)・`GET
  /api/feature-flags/:name/evaluate?bucket_key=...`(evaluate、
  flag自体が未知なら404)、`FEATURE_FLAG_REQUEST`jsonschemaバリデータ、
  テスト9本。**実バイナリ+curlで全経路を検証**(self-issueキー取得→
  upsert→get→list→evaluate→evaluate-unknown(404)→キーなし(401)→
  delete→get(404)、すべて期待通り)。`cargo test --workspace`green確認後
  commit・push(`e283df0`)。
  **(3) poem-cosmo-tauriへ逆方向ミラー**: `open-runo-feature-flags`
  crate・型・`AppState`配線・ハンドラ5本・テスト9本をそのまま移植
  (ClickHouse Debug impl事例と同種の、開発が先行した側への逆方向ミラー)。
  `cargo check --workspace`が1回で通り、`cargo test --workspace`
  (open-runo-router: 80→89テスト)green確認後commit・push(`23c3f7d`)。
  **(4) poem-cosmo-tauriからこちらへ通常方向ミラー**: (1)のDB型集約が
  このリポジトリにまだ反映されていなかったことを`grep`で確認、同じ集約
  +`open-runo-cli`の`db`サブコマンドをこちらにも移植。`cargo test
  --workspace`green・`apps/desktop-wasm`の`wasm32-unknown-unknown`
  ビルドも確認後commit・push(`85a16a7`)。
  `docs/cosmo-parity.md`4a節のFeature Flags行を両リポジトリで
  取り消し線+「✅ 完了」に更新。
  次回パスがすべきこと: `docs/cosmo-parity.md`4a節の残りのギャップ
  (EDFS/Kafka連携、gRPC Connect対応、MCP Server統合、いずれも
  ★★☆以下・実装コスト大)から次の実用性向上タスクを選び、poem-cosmo-
  tauriで先行実装した上でこちらへミラーを継続(ユーザー指示により確認
  不要で自動継続)。

- **2026-07-11 Mirror mongodb featureのコンパイルエラー修正を
  poem-cosmo-tauriから**: `mongodb`クレート2.x→3.7のAPI変更
  (`replace_one`/`find_one`/`delete_one`/`find`がbuilderパターンに
  変更、オプションは`.upsert(true)`/`.sort(doc!{...})`のメソッド
  チェーンで指定)に追従できていなかった`crates/open-runo-db/src/lib.rs`
  の`mongo`モジュールを修正(`cargo check -p open-runo-db --features
  mongodb`で再現・修正確認)。**`ClickHouseBackend`の`Debug`derive
  バグはこのリポジトリでは既に(`993af66`で)修正済みのため今回の
  ミラー対象外**(poem-cosmo-tauri側がこちらより遅れていたため、
  そちらへ逆方向でこの修正を移植したのが前回パス — 詳細は
  poem-cosmo-tauriの同日CLAUDE.md HANDOFFエントリ参照)。
  `cargo check -p open-runo-db --all-features`
  (postgres/mysql/sqlite/aruaru/cockroach/yugabyte/mongodb/surrealdb/
  redis/clickhouse全部同時)・`cargo test --workspace`
  (全33テストバイナリ)ともgreenを確認。
  次回パスがすべきこと: (1) `docs/api-examples.md`のCoverage note通り
  残り約25エンドポイントへのOpenAPIスキーマ自動生成拡大、(2)
  `docs/cosmo-parity.md`4a節の残りのギャップ(EDFS/Kafka連携、gRPC
  Connect対応、Feature Flags、MCP Server統合)から次の実用性向上
  タスクを選ぶ(ユーザー指示により確認不要で自動継続)。

- **2026-07-11 Mirror OpenAPIスキーマ自動生成 + CORS preflightバグ修正を
  poem-cosmo-tauriから**: `open-runo-api-types`の5型に
  `schemars::JsonSchema`derive追加、`openapi.rs`の`components.schemas`を
  スキーマ登録/フェデレーション系エンドポイント分だけ手書きJSONから
  自動生成に変更(`openapi-typescript`等でこの仕様からTS型生成する
  際、実際のRust構造体とdriftしなくなる)。
  **本セッション最大の実害バグ修正**: `build_hyper_app`が登録する
  約30ルート全てが自分のメソッドしかOPTIONSを登録しておらず、
  `Router::dispatch`の405フォールバック(ミドルウェア到達前)が
  CORS preflightを握りつぶしていた — 非simpleヘッダ(X-Api-Key)を送る
  クロスオリジンのブラウザ呼び出しは保護エンドポイント全てで常に
  失敗していた。`Router::with_cors_preflight()`で修正、
  `build_hyper_app`の最後に追加。新規`docs/api-examples.md`
  (vanilla JS例・openapi-typescript手順・HTML+Bootstrap例)も追加。
  詳細・実クロスオリジンブラウザでの再現・修正確認結果は
  poem-cosmo-tauriの同日CLAUDE.md HANDOFFエントリを正とする。
  **ミラー時の注記**: `Cargo.toml`の丸ごとコピーで、前パス
  (`993af66`)がFederatedBackend TOML設定化のために追加した
  `toml = "0.8"` workspace依存が消えてしまう事故があったため、
  コピー後に手動で復元(`crates/open-runo-db/src/federated_config.rs`
  が`toml::from_str`を使うため、これが無いとビルド不能になるところ
  だった — `cargo check --workspace`で実際に検知・修正して確認)。
  `cargo check --workspace` / `cargo test --workspace`
  (FederatedBackend分含む全33テストバイナリ)ともgreenを確認。
  次回パスがすべきこと: (1) `docs/api-examples.md`のCoverage note通り、
  残り約25エンドポイントへのスキーマ自動生成拡大を検討、(2) `mongodb`
  featureのコンパイルエラー修正(別タスク切り出し済み)、(3)
  `docs/cosmo-parity.md`4a節の残りのギャップから次の実用性向上タスクを
  選ぶ(ユーザー指示により確認不要で自動継続)。

- **2026-07-11 Mirror request-id相関 + rate-limit UX統合をpoem-cosmo-tauri
  から**: `open-runo-security::RateLimiter::seconds_until_reset`追加、
  `middleware_hyper::with_tracing`がX-Request-Id自動生成/echo+ログ記録、
  `with_shared_rate_limit`が429で`open-runo-api-types::
  RateLimitedResponse`(新規共有型)+`Retry-After`ヘッダを返すように変更。
  `apps/desktop-wasm/src/api.rs`と`open-runo-cli`の両方がエラー時に
  request-idを表示・429時に「rate limited, retry in Ns」という親切な
  メッセージを表示するよう更新(CLIは`self_issue_key`とdecodeで
  エラー整形ロジックを共有する`check_status`ヘルパーに統合)。
  **ミラー作業時の注記**: `git fetch`した時点で別プロセス(FederatedBackend
  TOML設定化+README全言語監査、コミット`993af66`)が既にこのリポジトリへ
  直接push済みであることを確認 —作業ツリーは既にそれを反映していたため
  コンフリクトなし、対象5ファイル(`apps/desktop-wasm/src/api.rs`・
  `crates/open-runo-api-types/src/lib.rs`・`crates/open-runo-cli/
  src/main.rs`・`crates/open-runo-router/src/middleware_hyper.rs`・
  `crates/open-runo-security/src/lib.rs`)のみをpoem-cosmo-tauriから
  コピーしてミラー(Cargo.toml/lock・README群・open-runo-db配下は
  このリポジトリ固有の変更なので上書きしていない)。詳細・実バイナリでの
  検証結果はpoem-cosmo-tauriの同日CLAUDE.md HANDOFFエントリを正とする。
  `cargo check --workspace` / `cargo test --workspace`(FederatedBackend
  分含む全33テストバイナリ)ともgreenを確認。
  次回パスがすべきこと: (1) ユーザーから新規指示 — HTML/CSS/JS/
  TypeScript/各種Bootstrap等Rust以外の言語・フレームワークからの
  呼び出しやすさ向上(OpenAPI経由のTS型生成・CORS再確認・vanilla
  fetch()利用例など、詳細はpoem-cosmo-tauri側HANDOFF参照)をpoem-cosmo-
  tauriで先行実装後、こちらへミラー、(2) `mongodb` featureのコンパイル
  エラー修正(前回パスで切り出し済み、別タスク)、(3)
  `docs/cosmo-parity.md`4a節の残りのギャップから次の実用性向上タスクを
  選ぶ(ユーザー指示により確認不要で自動継続)。

- **2026-07-11 FederatedBackend の TOML 設定化 + README 全言語の正確性監査**:
  poem-cosmo-tauri 側の最新コミット(`db09d1d` Add open-runo-cli)はこちらの
  `6f959aa`(同日中に確認時点でのHEAD)で既にミラー済みだったため、
  今回はミラー待ちの未反映作業なし。`todo!()`/`unimplemented!()`/stub
  マーカーも見つからなかったため、`docs/HANDOFF.md`「次セッション候補」の
  **FederatedBackend の設定ファイル化(TOML で members/routes を宣言)**を
  このリポジトリで直接実装した(詳細は`docs/HANDOFF.md`参照): 新規
  `open-runo-db::federated_config`モジュール(`FederatedConfig::from_file`
  / `from_toml_str` / `connect().await`)、ワークスペース依存に
  `toml = "0.8"`追加、テスト5件追加(`open-runo-db`は27テスト成功)。
  検証中に見つけた既存バグ2件も対応: (1) `clickhouse` feature 有効時に
  `ClickHouseBackend`への`#[derive(Debug)]`が`clickhouse::Client`の
  Debug未実装でコンパイル不能だったのを手動`Debug`implに修正
  (`--features full`が通ることを確認)。(2) `mongodb` feature がmongodb
  クレート3.7系のAPI変更(`find_one`/`delete_one`/`find`の引数減)で
  コンパイル不能なのを発見したが、デフォルトfeatureには影響せずスコープ外
  のため今回は修正せず、別タスクとして切り出し済み(次回パスが拾える)。
  あわせてREADME全10言語+ルートを実装内容と突き合わせて監査: ルート
  `README.md`・`README-Japan.md`・`README-English.md`の3ファイルが
  過去のミラー作業で poem-cosmo-tauri 自身のREADMEをそのまま上書き
  コピーされてしまっており、タイトルが「poem-cosmo-tauri」・
  `git clone`先が誤って`poem-cosmo-tauri`リポジトリを指す、という実害の
  ある誤りだったため修正(タイトル/clone先をopen-runo自身に戻し、
  poem-cosmo-tauriへは姉妹リポジトリとして正しく言及する形に変更)。
  さらに全10言語で共通して古い情報だったクレート数(15→**17**、
  `open-runo-cli`/`open-runo-api-types`を含む)・テスト数(192→**210**、
  実測値)を修正。中/韓/西/仏/独/伊/露/アラビア語の8言語は「Rust + Poem
  製フレームワーク」「Tauri 2 デスクトップアプリ(TypeScript + Bootstrap
  5)」という2026-07-10方針転換前の古い記述のままだったため、Poem/Tauriを
  直接依存させない現行アーキテクチャの説明とRust→WebAssembly構成に修正、
  あわせて`open-runo-cli`とFederatedBackendのTOML設定への言及を追加。
  なお本パス作業中に別プロセス(同種の自動メンテナンスジョブと思われる)が
  このリポジトリへ直接コミット・push(`08e10a7` Mirror open-runo-api-types
  from poem-cosmo-tauri)しているのを確認、作業ツリーは自動的に追随して
  おり本パスの変更との衝突は無かった。`cargo check --workspace` /
  `cargo test --workspace`(17クレート、210テスト)ともgreenを確認して
  push。次回パスがすべきこと: (1) `mongodb` feature のコンパイルエラー
  修正(上記・別タスク切り出し済み)、(2) `docs/cosmo-parity.md`4a節の
  残りのギャップ(EDFS/Kafka連携、gRPC Connect対応、Feature Flags、MCP
  Server統合)から次の実用性向上タスクを選ぶ、(3) Google Drive API
  直接統合(OAuthデバイスフロー)、per-field `@cacheControl`、JWKS
  定期リフレッシュ(いずれも`docs/HANDOFF.md`次セッション候補より)。

- **2026-07-11 Mirror open-runo-api-types(router/CLI/WASM共有型)を
  poem-cosmo-tauriから**: 新規`crates/open-runo-api-types`
  (17クレート目、`serde`のみ依存・I/Oなし・wasm32対応)に
  `SchemaVersion`・`RegisterSchemaRequest`・`SchemaHistoryResponse`・
  `FederationStatusResponse`を集約 — 同じ"スキーマバージョン"形状が
  router(`handlers_hyper.rs`)・`apps/desktop-wasm/src/api.rs`・
  `open-runo-cli`の3箇所で独立再定義されdriftしていた問題(登録
  レスポンスがsdl欠落、フロントエンドのhistory用structがnamespace+sdl
  欠落)を解消。3箇所すべてをこの共有crateに向けて書き換え、
  `apps/desktop-wasm`(独立workspace)には相対パス依存を追加。
  副次効果として`POST /api/schemas`のレスポンスに`sdl`が追加
  (後方互換)、Schema HistoryページにもnamespaceがUI表示されるように
  なった。詳細・実CLI+実ブラウザでの統合動作確認(CLIで登録した
  スキーマが同一UUID/タイムスタンプでブラウザのSchema Historyページに
  実際に見えることを確認)結果はpoem-cosmo-tauriの同日CLAUDE.md
  HANDOFFエントリを正とする(実装・検証はそちらで先行、このリポジトリ
  へは`crates/open-runo-api-types/` / `Cargo.toml` / `Cargo.lock` /
  `apps/desktop-wasm/{Cargo.toml,Cargo.lock,src/api.rs,src/pages.rs}` /
  `crates/open-runo-cli/{Cargo.toml,src/main.rs}` /
  `crates/open-runo-router/{Cargo.toml,src/handlers_hyper.rs}`を
  そのままコピーしてミラー)。`cargo check --workspace` /
  `cargo test --workspace`(全33テストバイナリ)ともgreenを確認。
  次回パスがすべきこと: (1) 同種のdrift問題が他のエンドポイント
  (DB CRUD・SCIM・Persisted Queries・Cache)にも無いか棚卸しし、価値が
  あれば同様に共有crateへ集約(poem-cosmo-tauriで先行実装した上で
  こちらへミラー)、(2) `docs/cosmo-parity.md`4a節の残りのギャップ
  (EDFS/Kafka連携、gRPC Connect対応、Feature Flags、MCP Server統合)
  から次の実用性向上タスクを選ぶ(ユーザー指示により確認不要で
  自動継続)。

- **2026-07-11 Mirror open-runo-cli(wgc相当CLI)をpoem-cosmo-tauriから**:
  新規`crates/open-runo-cli`(16クレート目、バイナリ`open-runo-cli`)を
  追加。`schema register/get/history`・`federation status`・
  `openapi`・`login`サブコマンド、`--api-key`省略時は自動self-issue。
  `clap`(derive+env)・`reqwest`をworkspace.dependenciesに追加。
  詳細・実バイナリでの動作確認結果はpoem-cosmo-tauriの同日CLAUDE.md
  HANDOFFエントリを正とする(実装・検証はそちらで先行、このリポジトリへ
  は`crates/open-runo-cli/` / `Cargo.toml` / `Cargo.lock` /
  `DEVELOPMENT.md` / `docs/cosmo-parity.md`をそのままコピーして
  ミラー)。`cargo check --workspace` / `cargo test --workspace`
  (全32テストバイナリ、open-runo-cli分含む)ともgreenを確認。
  次回パスがすべきこと: `docs/cosmo-parity.md`4a節の残りのギャップ
  (EDFS/Kafka連携、gRPC Connect対応、Feature Flags、MCP Server統合)
  から次の実用性向上タスクを選び、poem-cosmo-tauriで先行実装した上で
  こちらへミラーを継続(ユーザー指示により確認不要で自動継続)。

- **2026-07-11 Mirror OTLP分散トレーシングexportをpoem-cosmo-tauriから**:
  `crates/open-runo-observability`にOTLP(OpenTelemetry Protocol) HTTP
  トレースエクスポータを追加(`init_tracing_with_otlp`、
  `OPEN_RUNO_OTLP_ENDPOINT`環境変数で有効化、未設定時は従来通り
  console-onlyのJSON tracing)。ワークスペースが宣言のみで未使用だった
  `opentelemetry`/`opentelemetry-jaeger`(0.22)を実際に使う
  `opentelemetry`/`opentelemetry_sdk`/`opentelemetry-otlp`(0.32)・
  `tracing-opentelemetry`(0.33)に置き換え。`open-runo-core::Config`に
  `otlp_endpoint`フィールド追加、`open-runo-router`/`open-runo-gateway`
  両`main.rs`を新関数呼び出しに切替。ついでに`.env.example`の
  `OPENRUNO_*`(アンダースコアなし、実コードと不一致で無効だった)を
  `OPEN_RUNO_*`に修正、`open-runo-core`のテストに並列実行時の
  env変数レースコンディションがあったのを`Mutex`で修正。詳細は
  `poem-cosmo-tauri`の同日CLAUDE.md HANDOFFエントリを正とする(実装は
  そちらで先行・動作確認済み、このリポジトリへは`.env.example` /
  `Cargo.toml` / `Cargo.lock` / `DEVELOPMENT.md` /
  `crates/open-runo-core/src/lib.rs` /
  `crates/open-runo-gateway/src/main.rs` /
  `crates/open-runo-observability/{Cargo.toml,src/lib.rs}` /
  `crates/open-runo-router/src/main.rs` / `docs/cosmo-parity.md`
  をそのままコピーしてミラー)。`cargo check --workspace` /
  `cargo test --workspace`(全32テストバイナリ)ともgreenを確認。
  次回パスがすべきこと: `docs/cosmo-parity.md`4a節の残りのギャップから
  次の実用性向上タスクを選び、poem-cosmo-tauriで先行実装した上でこちらへ
  ミラーを継続(ユーザー指示により確認不要で自動継続)。

- **2026-07-10**: 定時の自律メンテナンスパス。`cargo check --workspace` /
  `cargo test --workspace --no-run` は変更前から成功済みを確認(ビルド破損なし)。
  `todo!()`/`unimplemented!()`/フェイクデータを返すスタブ関数は見つからず
  (実装は本当に完了している)。README-Japan.md と README-English.md が
  Phase A 以前の古い「ビジョン文書」のまま放置されており、実際の実装
  (15クレート・176テスト・自己学習AI・KeyGuardian・DUAL DATABASE・
  VersionlessAPI 等)と矛盾していた(例: 英語版は「設計・開発初期段階」
  「License TBD」「外部LLMプロバイダへのルーティング」と記載)ため、
  ルートの `README.md`(正しい最新情報)を基準に両ファイルを修正した:
  README-Japan.md はルート README.md の内容をそのまま反映、
  README-English.md は他8言語版と同じ構成(機能比較表・open-runo限定機能・
  クイックスタート・15クレート構成)の正確な英語版に書き換えた。
  他8言語版(中/韓/西/仏/独/伊/露/アラビア語)は内容確認済みで正確、変更不要。
  次回パスへの引き継ぎ: 特に緊急の課題は残っていない。次点候補は
  `docs/HANDOFF.md` の「次セッション候補」(Google Drive API 直接統合、
  FederatedBackend の TOML 設定化など)。
