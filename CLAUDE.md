# 開発方針・開発環境ルール(全リポジトリ共通ヘッダー、2026-07-15追記)

## 1. 比較的新しい言語・フレームワークの参照資料一覧

Rust自体は歴史があるが、本エコシステムが採用する **Poem** のような
比較的新しい・情報量がまだ少なめのWebフレームワークは、Python+FastAPIの
ような広く普及した組み合わせと比べ、AIモデルの学習データ・公開されている
実装例/Q&A/ブログ記事の絶対量が少ない傾向がある。そのため、AI駆動開発
(Claude等)がこれらを扱う際、実装の勘違い・API名の記憶違い・古いバージョン
のAPIでの実装(本プロジェクトで実際に複数回発生した既知の失敗パターン)に
よる**手戻り・いたちごっこ**が起きやすい。

対策として、AIが作業を始める際は、以下から**そのタスクに必要な部分だけ**を
先に参照してから実装に着手すること(全部読む必要はない。関連しそうな1〜2件を
拾い読みする程度で十分)。これにより歩留まりが上がり、AI駆動開発の手戻りが
減ることが期待される。

| 技術 | 公式ドキュメント | GitHub | 補足・ブログ等 |
|---|---|---|---|
| Rust言語本体 | https://doc.rust-lang.org/book/ | https://github.com/rust-lang/rust | https://blog.rust-lang.org/ |
| Poem(Webフレームワーク) | https://docs.rs/poem/latest/poem/ | https://github.com/poem-web/poem | https://crates.io/crates/poem |
| Tokio(非同期ランタイム) | https://tokio.rs/tokio/tutorial | https://github.com/tokio-rs/tokio | https://tokio.rs/blog |
| async-graphql | https://async-graphql.github.io/async-graphql/en/index.html | https://github.com/async-graphql/async-graphql | https://crates.io/crates/async-graphql |
| Tauri | https://tauri.app/ | https://github.com/tauri-apps/tauri | https://tauri.app/blog/ |
| wasm-bindgen / web-sys | https://rustwasm.github.io/wasm-bindgen/ | https://github.com/rustwasm/wasm-bindgen | https://rustwasm.github.io/docs/book/ |
| SurrealDB | https://surrealdb.com/docs | https://github.com/surrealdb/surrealdb | https://surrealdb.com/blog |
| sqlx | https://docs.rs/sqlx/latest/sqlx/ | https://github.com/launchbadge/sqlx | |
| WinFsp | https://winfsp.dev/ | https://github.com/winfsp/winfsp | |
| DirectX 12 / DirectML | https://learn.microsoft.com/en-us/windows/win32/direct3d12/directx-12-programming-guide | https://github.com/microsoft/DirectML | https://devblogs.microsoft.com/directx/ |
| WebAssembly(wasm32全般) | https://webassembly.org/ | https://github.com/WebAssembly | https://rustwasm.github.io/docs/book/ |

> ⚠️ **重要な注意(正直な開示)**: このURL一覧は、Web検索ツールを持たない
> セッションで学習データに基づき記載したものであり、**実在性・現在の
> 有効性・記載内容の正確性を検証していない**。特にAI(Claude含む)が
> このリストを鵜呑みにして実装や回答の根拠にすることは避け、
> **開発者自身が実際にアクセスして確認する**か、Web検索が使える
> セッションで一次情報を再確認してから利用すること。リンク切れ・
> リダイレクト・バージョン変更(特にAPIの破壊的変更)の可能性を
> 常に考慮する。新しい技術を追加する場合はこの表に追記していくこと。

## 2. AI駆動開発ツールに関する所感(2026-07-15、ユーザー所感として記録)

2026-07-15時点、ChatGPT等の汎用AIチャットは小規模なWebアプリ程度までは
開発できるものの、システムがある程度複雑・大規模になると出戻りが大きくなり、
一度に扱えるプログラムサイズにもすぐ限界が来る傾向がある。

Claude Code / Claude Desktopは、ローカルドライブを直接指定してファイルの
読み書きができ、GitHubリポジトリの読み出し(本プロジェクトのような
複数リポジトリにまたがるエコシステム)にも対応できるため、本プロジェクトの
ような規模のAI駆動開発には適していると考えられる。新しくAI駆動開発環境を
セットアップする際の選択肢として推奨する。

---

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

### パフォーマンス・並行処理方針(2026-07-13、ユーザー指示)

システム全体として、4層4重の通信・DB冗長化によるハイセキュリティを
保ちつつ、ハイパースレッディング/マルチコア/マルチスレッドを活かした
高速性を両立させる。**非同期(tokio、マルチスレッドランタイム)を基本**
とし、必要な場面(CPU負荷の高い計算・厳密な順序保証が必要な処理等)での
み同期処理を用いる。着眼点: (1) `#[tokio::main]`のランタイムflavorが
current_threadに固定されていないか、(2) async関数内でのブロッキング
I/O・CPU負荷処理は`tokio::task::spawn_blocking`へ退避、(3) CPU律速な
処理は`rayon`等でのデータ並列化を検討、(4) セキュリティクリティカルな
ホットパスの排他ロックがボトルネックになっていないか、を確認する。

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
- **open-easyweb**(第二のKUSANAGI、ドメイン/サブドメイン簡単登録+HTTPS
  自動監視/発行/更新の易操作ツール。高速化機能は含まない、2026-07-13に
  aruaru-webから分離): https://github.com/aon-co-jp/open-easyweb
- **aruaru-web**(2026-07-13廃止。易操作機能はopen-easyweb、高速化機能は
  このリポジトリ/poem-cosmo-tauriへ分割継承済み): https://github.com/aon-co-jp/aruaru-web
- **open-raid-z**(開発ルールの正本): https://github.com/aon-co-jp/open-raid-z
- **rs-to-readme**: https://github.com/aon-co-jp/rs-to-readme

## Web高速化機能の開発方針(2026-07-13、aruaru-webから継承)

2026-07-13付けで、`aruaru-web`が開発していたKUSANAGI風のWeb高速化機能
(gzip圧縮・静的アセットの長期キャッシュ・FastCGIバッファ調整・
upstream keepaliveプーリング)の開発をこのリポジトリ(および
poem-cosmo-tauri)が引き継いだ。ただしNginx/Apache設定生成という
アプローチはとらず、**ネイティブRust実装(hyperミドルウェア)として
提供する**方針(このエコシステムの「外部フレームワークに依存せず
自前実装する」という一貫方針の延長)。gzip応答圧縮は既存の
`with_compression`ミドルウェアが既にカバー済み、静的アセットの
長期キャッシュは新規`with_static_cache_headers`
(`crates/open-runo-router/src/middleware_hyper.rs`、`Cache-Control:
public, max-age=N, immutable`)で対応済み。FastCGIバッファ調整・named
upstream keepaliveプーリングはNginx固有のリバースプロキシ実装詳細で
あり、このリポジトリ自体がNginxの代替となるRustサーバーであるため
(Nginxの手前に立つ別のプロキシではないため)、移植すべき同等の概念が
無いと判断——詳細はこのCLAUDE.mdのHANDOFF該当エントリ、または
`open-easyweb`のCLAUDE.mdを参照。

**(2026-07-13、ユーザー指摘を受けた再検討・結論の一部訂正)**: ユーザーから
「Apache HTTPD(`mod_proxy_http`/`mod_proxy_ajp`)がTomcatの手前に立ち、
TLS終端・静的アセットオフロード・複数Tomcatへの負荷分散・**プロキシ→
バックエンドAPサーバー間のコネクションプーリング/keepalive**を担う」
という具体的な類推で、上記の結論の再検討を指示された。日本語・英語
両方で調査した結果("Rust hyper server behind nginx reverse proxy
production best practice"、"tokio async server 複数プロセス
ロードバランス デプロイ"等)、結論は**部分的に訂正**する:

- **上記の「移植すべき同等の概念が無い」という結論のうち、FastCGI
  バッファ調整・named upstream keepaliveプーリングそのものについては
  引き続き正しい**——これらはTomcatの**スレッドプアモデル**
  (リクエストごとに専用OSスレッドをブロックする、同時接続数がスレッド
  プールサイズに直結する)が生む問題への対症療法であり、Apache側の
  バッファリング・接続管理でTomcatの限られたワーカースレッドを
  食い潰されないよう保護する必要があった。tokio/hyperの非同期I/O
  モデルは1接続1スレッドを消費しないため、この特定の問題(スロー
  クライアント対策としての手前でのバッファリング)は根本的に存在しない。
  これは今回の調査で裏付けが取れた(2026年時点の実務知見・GitHub上の
  axum/hyper本番デプロイ事例でも、nginx/Caddy/Envoyを前段に置く主な
  理由はTLS終端の簡便さ・複数マシンにまたがる水平スケーリング・
  ゼロダウンタイムデプロイであり、「スロークライアントからバックエンドを
  守るための接続プーリング」目的での記述は見当たらなかった)。
- **一方、「クラスタ化・複数インスタンスでのロードバランス配下運用」
  という運用パターン自体は、tokio/hyperサーバーでも実務上ごく一般的**
  であり、この部分の価値をこれまで過小評価していた
  (ユーザーの類推が正しく指摘した点)。ただし理由はTomcatの場合と異なる:
  接続保護のためではなく、(a) TLS終端をアプリプロセスの外に出す運用上の
  簡便さ、(b) 単一マシンの限界を超えた複数マシンへの水平スケーリング
  はアプリ自身にクラスタリングを実装するより外部LBに任せる方が単純、
  (c) ローリング再起動時にLBがインスタンスをローテーションから外す
  ことでのゼロダウンタイムデプロイ、が真の技術的根拠。
- **この再検討に基づき、今回実装したもの**(詳細は本ファイルの
  HANDOFF最新エントリ参照): (1) `hyper_compat::serve_with_shutdown`
  +`shutdown_signal()`によるSIGTERM/SIGINTでのgraceful shutdown
  (`open-runo-router`・`open-runo-gateway`両バイナリのmain.rsに配線、
  実際にin-flightリクエストがシャットダウン後も完了することを証明する
  実テスト付き)——LBがインスタンスをローテーションから外して
  ローリング再起動する際に必須。(2) `GET /health`/`/healthz`が
  常時200を返すだけだった実バグを修正、実際に`DbBackend::list()`を
  呼んでバックエンド接続性を確認するように変更(LBのヘルスチェックが
  常に「healthy」を返すだけでは、クラスタ化の意味が無いため)。
  (3) `docs/deployment-scaling.md`に、nginx/Caddyを前段に置いた
  複数インスタンス運用の具体的な構成例(ヘルスチェック・keepalive
  設定含む)を新規作成、`PORTING.md`から参照。
- **実装しなかったもの・理由**: FastCGIバッファ調整・named upstream
  keepaliveプーリングの**Rustネイティブ移植**は、上記の通りtokio/hyper
  モデルには対応する問題が存在しないため、依然として実装しない
  (この部分の結論は維持)。マルチインスタンス対応のセッション/
  レートリミット状態共有(現状はプロセス内メモリ、複数インスタンス間で
  非共有)は次回パスの候補として残す——今回は`open-runo-observability`
  のClickHouseシンクのような外部ストア連携の枠組みはあるが、
  レートリミッタ自体を外部ストア(Redis等)へ移行する変更は本パスの
  スコープ外と判断(影響範囲が広く、単独のフォーカスされたパスとして
  実施すべき)。

## 運用ルール

- **開発中はこの`CLAUDE.md`を、コード変更のコミット/pushと必ず一緒に
  push する**(内容を更新した場合はもちろん、変更が無い場合も他の変更と
  一緒にコミット対象へ含めておくこと)。
- 実装で迷った場合や、API仕様の詳細確認が必要な場合は、学習データからの
  推測より公式ドキュメント(上記URL)を優先して参照する。
- 作業ドライブが変わった場合は、この節を更新し、関連プロジェクトの
  引き継ぎ資料にも変更の経緯を記録すること。
- **ローカル作業ドライブ(`F:\open-runo`)上の各リポジトリは、常にリモート
  (GitHub)の最新コミットに追従させておくこと**(`git fetch`/`git pull`を
  こまめに実行する。ローカルにのみ存在する未コミット変更がある場合は、
  上書き前に必ず内容を確認し、必要なら `git stash` で退避してから最新化
  する)。
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

- **2026-07-15 コードヘルス監査(6リポジトリ横断)— audit only, no changes**:
  `cargo build --workspace`/`cargo test --workspace`を実行し、警告3件
  (`hyper_compat.rs`の`missing_debug_implementations`、既存の既知警告)
  のみでビルド成功・全343テストgreen(failed 0)を確認。`git status`は
  クリーン、修正すべき壊れたビルド・失敗テスト・小規模な欠落は見つから
  なかったため、コード変更は行っていない。**気づいた点(修正はしていない、
  次回検討候補)**: このリポジトリのCLAUDE.mdは「Tauri・Poem・WunderGraph
  Cosmoを外部パッケージとして直接依存させない」方針を明記しているが、
  実際には`cargo build`のコンパイル出力に`poem`/`async-graphql-poem`
  クレートが含まれている(依存グラフ上に存在)——ドキュメントの方針と
  実際の依存関係の間に drift がある可能性があるため、次回パスで
  `cargo tree`等による依存関係の実態確認を推奨する。

- **2026-07-14 RJSON Phase 2 着手をpoem-cosmo-tauriからミラー(サーバー側
  部分抽出のコア実装完了) — ユーザー指示により本日はここで停止・
  ドキュメント整理してpush**: poem-cosmo-tauri側で実装・テスト済みの
  `open_runo_rjson::extract_path`(ドット/ブラケット記法のパス言語、
  `stats.damage`・`bonuses[1]`・`items[2].name`、外部パースクレート
  非依存の自前実装)と`DbBackend` traitへの`get_field`デフォルト実装
  (`crates/open-runo-db/src/lib.rs`)をこちらへミラー。
  `cargo test -p open-runo-rjson`(25件、17→25に増加)・`cargo test -p
  open-runo-db --features rjson rjson`(4件)ともgreenを確認済み。
  `docs/rjson.md`もpoem-cosmo-tauriと同内容に同期。
  **次回セッションが最初に行うべきこと(詳細はpoem-cosmo-tauri側の
  同日CLAUDE.md HANDOFFエントリが正)**: (1) `RjsonBackend`の
  `get_field`実装(`extract_path`は完成済み、配線のみ)、(2)
  `open-runo-router`へのRESTエンドポイント配線(`GET
  /api/db/:table/:key/field?path=...`)、(3) 上記完了後Phase 3
  (永続化・レプリケーション・インデックス化)の規模見積もり。
  次回パスがすべきこと: 上記(1)→(2)→(3)の順で着手(確認不要で実装を
  進める既存の運用ルールに従う)。

- **2026-07-14 gRPC reflectionの`file_containing_symbol`対応を
  poem-cosmo-tauriからミラー + 10ヶ国語READMEテスト数再同期**:
  poem-cosmo-tauri側で新規実装された`grpc.health.v1.Health`用の手書き
  `FileDescriptorProto`(`extract_file_containing_symbol`・
  `build_health_file_descriptor_proto`・`resolve_symbol_file_descriptor`・
  `encode_file_descriptor_response`)を`grpc.rs`ごとそのままコピーして
  ミラー、`cargo test -p open-runo-router grpc::`で27件全green(修正
  不要)。`docs/poem-parity.md`も同期。10ヶ国語READMEのテスト数バッジを
  337/356→343/362に更新(`cargo test --workspace --all-features`が
  343→362テストに増加したことを確認)。両リポジトリともcommit・push済み
  (poem-cosmo-tauri`902784f`+`4142559`、open-runo`73f69b2`+`f6e7c53`)。
  次回パスがすべきこと: 特に緊急の課題は無い。ACME(3チャレンジ型)・
  MCP Server(Tools/Resources/Prompts)・gRPC(unary/streaming/
  reflectionのlist_services+file_containing_symbol)はpoem-cosmo-tauriと
  こちらの両方で同期済み。次に高価値なタスクが必要な場合は
  `docs/cosmo-parity.md`の残りギャップを検討。

- **2026-07-14 poem-cosmo-tauriから3件ミラー(MCP Prompts・ACME
  TLS-ALPN-01/DNS-01) + `--all-features`ビルド破損の実バグ修正 +
  gRPCストリーミング/reflectionの未ミラーdrift是正 + ドキュメント最終
  同期**: スマホ経由セッションでの作業再開時、`cargo test --workspace
  --all-features`が**コンパイルエラーで失敗する**実バグを発見
  (`edfs.rs`が使う`anyhow`クレートが`open-runo-router/Cargo.toml`に
  一切宣言されていなかった)——`edfs` feature経由のoptional dependency
  として追加して修正(`d7a2050`)。続けてpoem-cosmo-tauri側で先行実装・
  検証済みだった3機能をミラー: (1) MCP Prompts
  (`prompts/list`/`prompts/get`、`summarize_api`が実際の`openapi::spec()`
  を動的レンダリング)、(2) ACME TLS-ALPN-01(RFC 8737、`tls_alpn01`
  サブモジュール、`ResolvesServerCert`によるALPN分岐)、(3) ACME DNS-01
  (RFC 8555 §8.4、`dns01`サブモジュール、`DnsProvider` trait +
  `CloudflareDnsProvider`)。3件とも実ファイルコピー後
  `cargo test --workspace --all-features`で個別に検証、全testgreen
  (`1d9245e`・`d826922`)。
  **ドキュメント最終同期パス中に発見した重大drift**: `docs/poem-parity.md`
  の記述だけを見て「gRPCストリーミング/reflectionは既に完了済み」と
  誤認しかけたが、実ファイル`crates/open-runo-router/src/grpc.rs`を
  poem-cosmo-tauri側(978行)とdiffしたところこちら側は393行しかなく、
  `Health/Watch`ストリーミング・`ServerReflection`(`list_services`)が
  **一度もミラーされていなかった**ことが判明——ドキュメントの記述と
  コードの実態は別々に確認すべきという教訓。`grpc.rs`をそのままコピーし
  ミラー、21件のgRPCテストが一発でgreen(修正不要)。
  10ヶ国語READMEのテスト数バッジ(307→337、`--all-features`で316→356)・
  クレート数(17→18、`open-runo-feature-flags`が一覧から漏れていたのも
  合わせて是正)、`PORTING.md`(`/mcp`行にPrompts追記、ACME段落を
  3チャレンジ型対応に更新、gRPC言及にストリーミング/reflectionを追記)を
  同期(`b623380`)。
  **検証中に遭遇した環境固有の既知パターン**: `cargo test --workspace`
  (デフォルトfeature)実行中に`LNK1201`(PDB書き込み失敗)で
  `open-runo-gateway`のリンクが1回失敗したが、`cargo clean -p
  open-runo-router`を挟んで再実行したところ成功——コード変更とは無関係な
  Windowsリンカーの一時的競合(過去のHANDOFFエントリに記録済みの
  既知パターンの再現)。最終的に337テスト全green(失敗ゼロ)を確認。
  次回パスがすべきこと: 特に緊急の課題は無い。ACME(3チャレンジ型全対応)・
  MCP Server(Tools/Resources/Prompts全対応)・gRPC(unary/streaming/
  reflection)はpoem-cosmo-tauriとこちらの両方で同期済み。次に高価値な
  タスクが必要な場合は`docs/cosmo-parity.md`の残りギャップを検討。

- **2026-07-13(VersionLessAPI+Gitハイブリッド読み出し側を再検証、依然green)**:
  `cargo test --workspace`(151件、全green、前回パスの報告通り)を再確認
  した上で、`crates/open-runo-db/tests/aruaru_as_of_commit.rs`の
  `#[ignore]`統合テスト(`as_of_commit_returns_the_old_value_through_the_
  real_pgwire_endpoint`)を、`aruaru-db`側で`cargo build -p aruaru-server`
  してから`cargo test -p open-runo-db --features aruaru --test
  aruaru_as_of_commit -- --ignored --nocapture`で実行し、**実際に
  aruaru-serverの実pgwireエンドポイントに接続して成功することを再確認**
  (ドリフト無し)。他パスがこの間に触れたコード(graceful shutdown修正等)
  との相互作用による劣化は無かった。
- **2026-07-13 直前コミット(`66bb5a7`)のgraceful shutdown実装に実バグ2件を
  発見・修正(mainブランチのテストが実際に壊れていた)**: 別の継続開発
  パスで`cargo test --workspace`を実行したところ、`open-runo-gateway`の
  GraphQLテスト3件が`ConnectionReset`で実際に失敗していることを発見。
  調査の結果、`serve()`ラッパーの「二度と発火しないshutdown」の実装に
  実際のバグがあった: 関数スコープの`watch::channel`のsenderを使って
  いたが、`serve()`がreturnした瞬間にそのsenderがdropされ、**dropされた
  `watch::Sender`はreceiver側の`changed()`を即座に解決させてしまう**
  ため、「決して発火しないはず」のshutdownシグナルが実際には直後に
  発火し、接続直後の全コネクションがリセットされていた。
  `std::future::pending()`(絶対に解決しないFuture)に置き換えて修正。
  さらに、この修正過程で`hyper_compat::tests::
  graceful_shutdown_lets_an_in_flight_request_finish_before_the_server_stops`
  テスト自体にも別の実バグを発見: `reqwest`の`.send()`が返すFutureは
  遅延評価であり、ローカル変数に束縛しただけでは何も実行されない
  (spawnもpollもされない限りHTTP接続自体が開始しない)。テストは
  shutdown発火**後**に初めて`request.await`していたため、実際には
  「in-flightリクエストの継続」を一切検証できていなかった
  (常にshutdown後に新規接続を試みて失敗する経路を通っていた)。
  `tokio::spawn`でリクエストを即座に開始するよう修正し、実際に
  「shutdown発火時点で処理中だったリクエストが正常完了する」ことを
  検証できるようにした。`cargo test --workspace`は全テストバイナリで
  failed 0(gatewayのGraphQLテスト3件・graceful shutdownテスト1件を
  含め151件)を確認。poem-cosmo-tauri側にも同じ2件の修正を実施済み
  (`efa5ad2`)。教訓: バックグラウンドパスが「実装完了・テストgreen」と
  報告しても、実際に`cargo test --workspace`を実行して確認するまでは
  信用しないこと(このセッションで複数回発生した既知のパターン)。

- **2026-07-13 Tomcat/Apache類推によるユーザー指摘を受け、リバースプロキシ
  配下運用の結論を再検討・部分訂正 — graceful shutdown + 実ヘルスチェック
  を新規実装**: 詳細な調査結論・根拠は本ファイル「Web高速化機能の開発方針」
  節の追記(2026-07-13)を参照。要約: FastCGIバッファ調整・named upstream
  keepaliveプーリングの「移植すべき同等の概念が無い」という結論自体は
  維持(Tomcatのスレッドプアモデル特有の問題であり、tokioの非同期I/Oには
  存在しない)。一方、複数インスタンスをリバースプロキシ配下でロード
  バランスする運用パターン自体は価値があると判断を訂正、以下を実装:
  (1) `crates/open-runo-router/src/hyper_compat.rs`に`serve_with_shutdown`
  +`shutdown_signal`(SIGINT/SIGTERM)を新規追加、`serve`は後方互換の
  ラッパーとして維持。`open-runo-router`/`open-runo-gateway`両
  `main.rs`をこちらへ切替。新規テスト
  `graceful_shutdown_lets_an_in_flight_request_finish_before_the_server_stops`
  (人工的に遅いハンドラでシャットダウン信号発火中のリクエストが実際に
  完了することを証明、かつシャットダウン後は新規接続を受け付けなく
  なることも確認)。(2) `hyper_compat::health_handler`が常時200を返す
  だけだった実バグを修正——`AppState::db`(`DbBackend::list()`)へ実際に
  問い合わせ、失敗時は503を返すよう変更(`GET /health`/`/healthz`の
  シグネチャに`Arc<AppState>`を追加、`lib.rs`の2箇所の呼び出し元・
  `hyper_compat.rs`内の既存テスト4箇所を追従修正)。(3) 新規
  `docs/deployment-scaling.md`(nginx配下でのN インスタンス運用レシピ、
  keepalive設定・ヘルスチェック設定含む)を作成、`PORTING.md`から参照。
  **検証状況(正直な限界)**: `cargo check --workspace`はgreen(既存の
  警告3件のみ、新規warning無し)。しかし本セッションのサンドボックス
  環境では、ループバックTCPを実際に張る`#[tokio::test]`(reqwest経由の
  実HTTPリクエスト)が**今回追加した分だけでなく既存の同種テストも
  含めて広範に`ConnectionReset (os error 10054)`で失敗する**環境固有の
  問題があることを確認した(`git stash`で変更前のコードに戻した状態で
  同じ`static_file_handler_serves_existing_file_and_404s_missing`
  (無変更の既存テスト)を単体実行しても同一エラーで失敗することを
  確認済み——本パスのコード変更が原因ではなく、このサンドボックスの
  ループバックTCP周りの制約(ファイアウォール/セキュリティソフト等)に
  起因すると判断)。よって「実際にgreenなテスト実行結果」という形での
  検証はこのセッションでは提示できない——次回パス(または別環境)で
  改めて`cargo test --workspace`のTCP依存テスト群を実行し、green確認
  すること。ロジック自体はhyperの標準的なgraceful-shutdownパターン
  (`graceful_shutdown()` + `tokio::select!`、`JoinSet`で全接続タスクの
  完了を待ってから`handle`が返る設計)に基づく。
  **未実装(次回パス候補として明記)**: レートリミット・セッション状態が
  プロセス内メモリのみで複数インスタンス間で共有されない
  (`docs/deployment-scaling.md`の「既知のギャップ」節に明記) — Redis等
  外部ストアへの移行は影響範囲が広いため本パスのスコープ外とした。

- **2026-07-13 コミットID指定の読み出しクエリAPI(open-web-server拡張
  要件(1)「VersionLessAPI + Git版管理ハイブリッド」の読み出し側)を実装 —
  `GET /api/db/:table/:key/at/:commit_id` を新規追加、aruaru-db側の
  ストレージ層実装(同日先行実装済み、`aruaru-query::engine::
  QueryEngine::select_as_of`)まで含めた実バイナリでの一気通貫検証済み**:
  aruaru-db側は同日別セッションで既に「AS OF COMMIT」SQL自体
  (`SELECT ... FROM t WHERE pk='v' AS OF COMMIT '<commit_id>'`)を実装
  済み(単一行のみ、詳細はaruaru-db側CLAUDE.md参照)だったが、
  open-runo/open-web-server側の配線が「未着手」と明記されていたため、
  このパスで着手・完成させた。
  - `crates/open-runo-db/src/lib.rs`: `DbBackend`トレイトに
    `get_at_commit(table, key, commit_id) -> Result<Option<String>>`を
    デフォルト実装(`AppError::Validation`で「未対応」を返す)付きで追加。
    `AruaruDbBackend`にのみ実装をオーバーライド。
  - **実装中に発見した2つの実バグ(いずれも今回のパスで修正)**:
    (1) `kv_store`の`PRIMARY KEY (table_name, key)`は複合キーだが、
    aruaru-db自身のSQLエンジン(`aruaru_query::QueryEngine`、実際に
    aruaru-serverのpgwireを裏で動かしている本体)は`WHERE`に単一の
    `col = 'val'`等価条件しか対応せず(`AND`未対応)、かつテーブルの
    Git-on-SQL的なPKは常に「先頭列の値」——`table_name`が先頭列である
    既存の共有DDL(`KV_STORE_DDL`)ではaruaru-db向けの
    `get`/`delete`/`upsert`が実は複合WHEREを要求しており、実バイナリに
    対しては最初から成立しない設計だったことが判明(PostgreSQL/MySQL等
    他バックエンドは実RDBMSなので複合WHEREが効くため、この不整合は
    aruaru-db固有かつ今まで気づかれていなかった)。**修正**:
    aruaru-db専用の`KV_STORE_DDL_ARUARU`(`migration.rs`)を新設し、
    `table_name || '\u{1}' || key`を合成した単一列`pk`を先頭列として
    追加、`put`/`get`/`delete`/`get_at_commit`は全てこの`pk`列への単一
    等価条件に統一(`list`は元々`table_name`単一条件のみで無変更)。
    (2) aruaru-wireの`ExtendedQueryHandler::describe_portal`が常に空の
    列リストを返す(動的スキーマのため、実`RowDescription`はExecute時
    にしか確定しない)ため、`sqlx`の拡張プロトコル(`query`/`query_as`+
    `.bind()`)経由で行データを持つ`SELECT`を投げると
    `ColumnIndexOutOfBounds`で失敗することを実バイナリ相手のテストで
    発見(`INSERT`/`DELETE`等コマンドタグのみの文は影響を受けない)。
    **修正**: `get`/`list`/`get_at_commit`(行データを返す3メソッド)は
    `sqlx::raw_sql`(シンプルクエリプロトコル、aruaru-wireの
    `SimpleQueryHandler`が正しく列データを返す経路)へ切り替え、
    リテラルは手動エスケープ。この2件はどちらも「commit_id読み出し」
    固有の問題ではなく、aruaru-dbバックエンドの通常の`get`/`list`も
    実バイナリに対しては壊れていた(=実際には一度もpgwire経由で
    エンドツーエンド検証されたことがなかった)ことを意味する潜在バグ
    ——今回のパスで副次的に修復できた。
    (3) `select_as_of`はSELECTの列リストを無視し常にフルROW
    (kv_storeの場合`pk, table_name, key, value`の4列)を返すことも
    実機テストで発見、`get_at_commit`はインデックス3(`value`列)を
    明示的に取得するよう対応。
  - `crates/open-runo-api-types/src/lib.rs`: `DbRecordAtCommitResponse`
    (`table`/`key`/`commit_id`/`value`)を新設。
  - `crates/open-runo-router/src/handlers_hyper.rs`・`lib.rs`:
    `db_get_at_commit_handler`+`GET /api/db/:table/:key/at/:commit_id`
    ルート登録。バックエンドが未対応の場合は501、コミット不明/その時点で
    キー未存在の場合は404を返す。
  - **検証(実バイナリでの一気通貫、型チェックのみでの「完了」報告では
    ない)**: `crates/open-runo-db/tests/aruaru_as_of_commit.rs`
    (`#[ignore]`、隣接リポジトリのビルド成果物に依存するため既存の
    他のクロスプロセス統合テストと同様デフォルト無効)が、実際に
    ビルドした`aruaru-server`バイナリを子プロセスとして起動し、実
    pgwireエンドポイント(`sqlx`、`AruaruDbBackend`が本番で使うのと
    同じクライアント)に接続、(1)`qty=1`でput→`aruaru_commit()`で
    commit_1発行、(2)`qty=5`に更新→再度commit、(3)最新値が`qty=5`である
    ことを確認した上で、(4)`get_at_commit`にcommit_1を指定すると
    **`qty=1`(最新値ではなく過去の値)が返る**ことを実証、(5)存在しない
    commit_idはエラーになることも確認。
    `cargo test -p open-runo-db --features aruaru --test
    aruaru_as_of_commit -- --ignored --nocapture`で実行・green確認済み。
    `cargo test --workspace`(デフォルトfeature、上記は`#[ignore]`なので
    含まれない)は既存の全テストfailed 0を維持、`open-runo-router`に
    追加した501確認テスト(`db_get_at_commit_reports_501_for_backends_
    without_commit_history`、InMemoryBackend相手)含め green。
  - **正直なスコープの限界**: (a) `open-web-server-gateway`への
    薄いpass-through配線は今回のパスでは未実施——
    `crates/open-web-server-gateway/src/handlers/`配下に既存の
    `/api/db/*`系プロキシパターンが1つも無く(既存実装は他ドメインの
    プロキシのみ)、新規パターンを前例なしに導入するのは
    open-web-server側の別セッション(同リポジトリで並行作業中の
    エージェントの作業領域`crates/open-web-server-wire/`とは非重複だが、
    設計判断の一貫性のため)で検討する方が適切と判断——次回以降の候補。
    (b) 全表スキャンの`AS OF`(単一PK以外)はaruaru-db側のストレージ層
    自体が未対応(aruaru-db側CLAUDE.md参照)。
  - 次回パスがすべきこと: (1) `open-web-server-gateway`への
    pass-through配線、(2) 全表スキャン`AS OF COMMIT`のaruaru-db側対応、
    (3) `docs/cosmo-parity.md`・`docs/api-spec.md`に新エンドポイントを
    反映。

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
## HANDOFF追記(2026-07-15) — §0.9「第二のApache/Tomcat/React」着手

- `docs/HYBRID_NETWORK_ARCHITECTURE.md` を **v1.0** に格上げ(§0.9新設:
  ポジショニング宣言と段階的ロードマップ。全6リポジトリに同一コピー配布、
  open-easyweb をscope追加)。
- 新クレート2つを追加し open-runo へ§0.5規則でミラー:
  - `crates/open-runo-appserver`(第二のTomcat骨格): `RuntimeProfile`
    (Rust+Poem/Python+FastAPI/PHP+Laravel/Ruby+Rails/Dart+Flutter雛形)、
    `Supervisor`(poll型tick、crash-loop指数backoff+give-up)、
    `Dispatcher` trait + `StaticDispatcher`(Host→upstream解決)。
  - `crates/open-runo-view`(第二のReact Phase 1): `VNode`/`h()`ビルダ、
    関数`Component<P>`、keyed reconciliation付き`diff()`→`Patch`列、
    SSR `render_html()`(エスケープ・void要素対応)。
    テストは「パッチ適用で旧→新ツリー一致」を機械検証。
- **検証方法**: sandbox cargo 1.75(edition2024制約)のため独立クレートとして
  `cargo test` 実施 — appserver 5/5、view 7/7 合格。workspace全体ビルドは
  PC側で `cargo build` にて要確認。
- 検証中に実バグを1件検出・修正: Supervisorのcrash時failureカウントが
  常に1にリセットされgive-upに到達しない問題 → `Running`状態に
  `prior_failures` を保持する方式に修正済み。
- **次ステップ(§0.9.3)**: view Phase 2(hooks相当+DOMアプライヤをopen-easyweb
  のwasm-bindgen側へ)、appserverのpoem統合Dispatcher、open-web-server
  TenantRegistry→StaticDispatcherアダプタ。

## HANDOFF追記(2026-07-15 第2弾) — Phase 2完了(hooks / HTTP転送 / マルチスレッドサーバ)

- **open-runo-view Phase 2**(`src/hooks.rs`): `Ctx::use_state`(型付きスロット+
  `Setter`のset/update)、`Ctx::use_effect`(depsハッシュ変化時のみ実行)、
  `Runtime<P>`(rerender→最小Patch列。2回目レンダリングがSetText 1件になる
  ことをテストで保証)。Reactと異なり暗黙グローバルでなく明示`Ctx`ハンドル方式
  (フック順序ルール自体はReact互換)。テスト10/10。
- **open-runo-appserver Phase 2**:
  - `src/proxy.rs`: std::netのみのHTTP/1.1転送 `proxy_once`(Host書き換え、
    X-Forwarded-Host/For付与、Content-Lengthボディ中継、Connection: close強制)。
    セキュリティ上限: ヘッダ16KiB/ボディ16MiB(超過は`ProxyError::TooLarge`)。
  - `src/server.rs`: `ThreadedProxyServer` — 固定ワーカースレッドプール
    (既定=論理CPU数)でproxy_onceを並列実行。**マルチCPU/マルチコア要件の
    直接実装**。キュー満杯時は黙殺せず503応答+統計カウント(§0監査性)。
    16並列クライアントの実ソケット統合テストで検証(3回連続グリーン)。
  - `src/tenant_bridge.rs`: open-web-server `TenantRegistry`との型非依存
    ブリッジ — `(host, backend_addr)`ペア列→`TenantDispatcher`(不変・
    ロック不要=読み取りスケール)。解析不能エントリは拒否リストで報告。
  - テスト10/10。flake 2件をテスト側で修正(時間ベース化・統計収束待ち)。
- **PC側の配線タスク**: open-web-serverのapp_proxy/tenant_routerから
  `dispatcher_from_tenants(registry.list()由来のペア列)`を呼ぶアダプタを
  gateway側に追加(クロスリポジトリ依存はCargo git依存 or パス依存を選択)。
  view Phase 3 = DOMアプライヤ(open-easyweb wasm-bindgen側)+SSR poem統合。

## HANDOFF追記(2026-07-15 第3弾) — view Phase 4(宣言的イベントバインド)完了

- **`VElement::events: Vec<(String, u64)>`** + `.on(event, handler_id)` builder。
  `diff()`が属性と同様に`SetHandler`/`RemoveHandler`パッチを生成(追加/変更/削除
  を検知するテスト付き)。
- **`hooks.rs::Ctx::use_handler(f)`**: 呼び出し位置(フック順序)に基づく
  安定`handler_id`を発行。`Runtime::dispatch(id)`でハンドラを起動
  (`Setter`経由で状態更新→`is_dirty()`→呼び出し側が`rerender`)。
  「handler_idはレンダリングを跨いで安定」「未登録IDのdispatchは無害な
  no-op」をテストで保証。テスト計17件(native、`--features dom`なし)。
- **`dom.rs`**: `DomMount::attach_with_dispatch(root_id, dispatch_fn)` —
  `DELEGATED_EVENTS`(click/input/change/submit)それぞれにルート1つの
  委譲リスナーを設置(Reactの合成イベント方式と同じ設計)。
  `event.target()`から祖先方向に`data-orv-<event>`属性を探索して
  `handler_id`を特定 → 呼び出し元の`dispatch_fn(id, event)`へ委譲。
  `Closure`はDomMountが保持し続ける(dropで失効するため)。
- **SSRにも`data-orv-<event>`属性を出力**するよう`render_into`を修正
  (hydration後に委譲リスナーが属性を発見できるようにするため必須。
  検証中に「SSR出力にイベント属性が漏れていた」実バグとして発見・修正)。
- **検証**: nativeテスト17/17(sandbox cargo 1.75)。`--features dom`は
  wasm-bindgen/web-sys を`=0.2.92`/`=0.3.69`に一時ピンしてローカル検証のみ
  `cargo check`成功を確認(コミットはピン無し版、open-easyweb側の既存API
  要求と衝突しないようレンジ指定`>=`のまま)。検証中に**実バグ2件**発見・
  修正: (1) web-sys 0.3.69で`Event`/`EventTarget`featureが未有効だった、
  (2) `JsCast`の重複import。wasm32ターゲット自体はsandboxに無く実機
  (ブラウザ)検証は未実施 — PC側で要確認。

## HANDOFF追記(2026-07-15 第4弾) — Poem gateway SSR統合(§0.9.3 Phase 3)

- 新モジュール `crates/open-runo-gateway/src/ssr.rs`: `open-runo-view::ssr::
  render_page`をPoemハンドラで`text/html`として返す薄い統合層。
  `GET /ssr/status`が`status_panel`(open-easyweb `view_bridge`と同一定義、
  §0.5ミラー契約——状態のJSON形状のみ共有し、コンポーネント本体は
  wasm32専用crateとネイティブPoemサーバで複製)をSSRし、
  `window.__OPEN_RUNO_STATE__`にhydration用JSONを埋め込み、
  open-easywebのwasmバンドルを読み込むscriptタグを出力する。
  `ssr_route()`を`Route::new().nest("/ssr", ssr::ssr_route())`で
  バイナリ側に組み込む想定。
  `open-runo-gateway`のCargo.tomlに`open-runo-view`を依存追加。
- **検証方法・既知の限界**: sandbox cargo 1.75は本crate単体でも
  `async-graphql-poem`(edition2024要求)でビルド不可という既存制約に加え、
  poem単体を切り出した独立チェックでもpoem 2.x/3.x双方の推移依存
  (`indexmap`最新版)がedition2024を要求し**取得ロックファイル生成の
  時点で**失敗するため、sandboxでは一切コンパイル検証できなかった
  (workspace全体はもちろん最小限の独立crateとしても不可——従来の
  「workspace lockfileが作れない」制約が今回は依存取得そのものに
  まで及んでいる新事実)。
  ソースコードは (a) 同crate内の既存`graphiql`ハンドラ・
  `graphql_route`のパターンを踏襲、(b) `poem::test::TestClient`の使用は
  同ファイル内`tests`モジュールの既存テスト(`health_field_resolves`)の
  実績あるAPIパターンを参考にしたが、`resp.0.into_body().into_string()`
  部分は類似実装からの類推であり**未コンパイル検証**。
  **PC側で`cargo test -p open-runo-gateway ssr::`を最初に実行し、
  API不一致があれば修正すること**(正直な開示)。

## HANDOFF追記(2026-07-15 第5弾) — poem/poem-derive/indexmap 厳密ピン(実MSRVバグ修正)

- **発見した実バグ**: 本ワークスペースは`rust-version = "1.75"`を宣言して
  いるにもかかわらず、`poem = { version = "3.1", ... }`(範囲指定)が
  実際にはpoem 3.1.12(Cargo.toml自体がedition2024要求)まで許容して
  しまい、**rust 1.75環境では原理的にビルドできない**状態だった。
  これはsandbox固有の回避策ではなく、CI等で`rust-version`をMSRVとして
  真面目に検証する場合に誰でも踏む実際の不整合。
- **対処**(`[workspace.dependencies]`): `poem = "=3.1.0"`、
  `poem-derive = "=3.1.0"`、`indexmap = "=2.2.6"`に厳密ピン。
  `open-runo-gateway/Cargo.toml`に`indexmap`を直接依存として追加
  (workspace.dependenciesへの追加だけでは、それを`{ workspace = true }`
  で直接参照していない推移依存には効かないため、resolverに古い版へ
  統一させるには直接依存としての明示が必要)。
- **検証**: `open-runo-gateway/src/ssr.rs`相当のコードを`poem-derive`と
  `open-runo-view`のみの最小依存構成で切り出し、`poem=3.1.0`/
  `poem-derive=3.1.0`/`indexmap=2.2.6`の組み合わせで
  **sandbox rustc 1.75上で実際にコンパイル・テスト合格**することを確認
  (前回HANDOFFで「未コンパイル検証」としていた
  `resp.0.into_body().into_string()`のAPI使用が正しいことも実証済み)。
- **意図的にスコープ外とした点**: `async-graphql`系(`async-graphql-derive`
  7.2.1がedition2024要求)、および`surrealdb-core`経由の`clap_builder`も
  同様にsandboxでは最新版がedition2024を要求するが、これらは
  Poem/SSR統合とは無関係な、ワークスペース全体の既存ドリフトであり、
  本セッションでは追いかけない判断をした(pinの連鎖が無関係な依存へ
  際限なく広がるため)。ワークスペース全体の`cargo check`は依然
  sandboxでは通らない(従来通りの既知制約)。**gatewayクレート単体を
  切り出した検証は上記の通り成功**。
- **検討したが採用しなかった案**: 「Poem本体を独自改変してupstreamへ
  push」——却下。(1) 問題の本質はPoemのコードでなくCargoの依存解決
  (推移依存の最新Cargo.toml自体がパース時にedition2024を要求し、
  実際に使うかどうかに関係なく解決過程で失敗する)ため、Poemを直しても
  解決しない。(2) github.com/poem-web/poemはaon-co-jp組織外の第三者OSSで、
  保有トークンのscope外かつ無断push は不適切。ピン留めという通常の
  Cargo運用で解決できることを優先した。

## アプリケーションサーバー層の役割(open-runo / poem-cosmo-tauri、2026-07-16追記)

「配信エンジン(vhost)」に`open-web-server`を選択肢として追加したが、
open-web-serverがApache＋Nginxのハイブリッド仕様のWebサーバーとして
まだ機能していない間は、Tomcatのような互換レイヤーとして機能するのは
`open-runo`または`poem-cosmo-tauri`である。

これらは`open-raid-z`とVersionlessAPIによって、バージョンレス運用と
バージョン管理・Git管理を両立しながら、ACID互換性とZFS互換性に対応した
`aruaru-db`と、PostgreSQLとのDUAL DATABASE構成による「4層4重」の
最新鋭の通信システムを構築し、仕様変更が容易なデータベース設計により、
3DオンラインゲームAI課金アイテム、オンライン金融、オンライン証券、
オンラインクレジットカード決済など、ネット上で紛失してはならない
ミッションクリティカルな用途向けに、24時間365日ノンストップの
サーバー対応WEBサイト開発を全面的にバックアップするフレームワーク・
ミドルウェアとして機能することを目指す。
