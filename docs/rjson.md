# RJSON

**Concept**: 石塚正浩(aon CEO)。**文法設計・実装**: Claude(2026-07-14)。

## これは何か

RJSON は、このエコシステム向けに設計した、人間が書きやすい JSON の
上位互換フォーマットです。**新しいデータモデルは導入しません**——
パース結果は標準の `serde_json::Value` そのものであり、このワーク
スペースの他のあらゆる JSON 消費コードがそのまま扱えます。RJSON が
提供するのは「厳密な JSON(RFC 8259)が拒否する、書きやすさのための
緩和」だけです。

## 文法拡張(strict JSON からの差分)

1. **トレイリングコンマ**: `[1, 2, 3,]`、`{"a": 1,}`
2. **コメント**: `// 行コメント`、`/* ブロックコメント */`
3. **クォート無しキー**: `{name: "sword"}`(識別子形状のキーのみ、
   `[A-Za-z_][A-Za-z0-9_]*`)
4. **シングルクォート文字列**: `'hello'`

意図的に**含めない**もの(将来検討、今回は文法をシンプルに保つ判断):
16進/8進数値リテラル、複数行文字列、`NaN`/`Infinity`数値リテラル、
YAMLのようなアンカー/参照。

4つの拡張はいずれも「strict JSON が表現できる値と全く同じ値」に
正規化されます——RJSON が JSON にできない新しい意味表現を持つことは
ありません。変わるのは**入力テキストへの寛容さ**だけです。

## 設計の系譜

拡張セット自体(トレイリングコンマ・コメント・クォート無しキー)は、
[JSON5](https://json5.org/)・
[JSONC](https://code.visualstudio.com/docs/languages/json#_json-with-comments)
という確立された慣習の、最小限の組み合わせです。新しい種類の緩和を
発明したわけではありません。実装は外部パースクレートに依存しない
完全な自前実装(`crates/open-runo-rjson`)——このワークスペースが
WebSocketフレーミング・multipartパース・gRPCのProtocol Buffers
コーデックで既に確立している「プロトコル/データ形状の層は手書き」
という方針をそのまま踏襲しています。

## 実装(Phase 1、2026-07-14)

- `crates/open-runo-rjson`: パーサー+シリアライザ本体。
  `parse(&str) -> Result<serde_json::Value, RjsonError>`・
  `to_string(&Value) -> String`(常に厳密な JSON を出力——RJSON の
  緩さは入力側のみの利便性で、保存・送信される正規形は常に曖昧さの
  無い標準 JSON)。17件のユニットテストで4拡張すべて+エラーケース
  (未終端文字列・未終端コメント・不正キー等)を検証済み。
- `crates/open-runo-db`(`rjson` feature): `RjsonBackend`——
  `DbBackend` trait実装。`put`時に`open_runo_rjson::parse`で検証し、
  失敗した値は書き込み拒否(**DB層での自動バリデーション**という
  ご提案の1つ目のメリットを実証)。保存されるのは常に正規化済みの
  strict JSON(コメント等は書き込み時に失われる——これは意図的な
  設計で、読み出し側は常に曖昧さの無い標準形を受け取る)。

## 実装(Phase 2、着手済み・2026-07-14)

**②ネットワーク帯域の節約(サーバー側での部分抽出)に着手**:
- `open_runo_rjson::extract_path(&Value, path: &str) -> Option<&Value>`
  — ドット/ブラケット記法のパス言語(`stats.damage`・`bonuses[1]`・
  `items[2].name`)でサーバー側の値から部分抽出する。外部パースクレート
  非依存の自前実装。8件のユニットテスト(単一キー・ネストキー・配列
  インデックス・複合パス・存在しないキー/添字・非配列への添字アクセス
  等)ですべて検証済み(`open-runo-rjson`は17→25テストに)。
- `DbBackend` traitに`get_field(table, key, path) -> Result<Option<String>>`
  のデフォルト実装(未対応→`AppError::Validation`)を追加。
- **未完了(次回セッションの最初のタスク)**: `RjsonBackend`側での
  `get_field`の実装(保存済みJSONを`extract_path`で部分抽出して返す)、
  および`open-runo-router`側の対応するREST エンドポイント
  (例: `GET /api/db/:table/:key/field?path=stats.damage`)の配線。
  `open-runo-rjson`クレート自体の`extract_path`は完成・テスト済みで
  すぐ使える状態——残るのは配線のみ。

## 今回のスコープ外(Phase 3以降、明示的な次回タスク)

ご提案いただいた3つのメリットのうち、①DB層での自動バリデーションは
完了、②サーバー側部分抽出は上記の通りコア実装は完了・DB/REST配線が
残っている状態です。以下は依然としてスコープ外として明記します:

- **③超高速な検索とインデックス化**: 現状はインメモリ`HashMap`
  (`InMemoryBackend`と構造的に同一)であり、永続化・レプリケーション・
  Git-on-SQL的なバージョン管理・インデックスは一切ありません。
  これらは`aruaru-db`(複数クレート・複数開発パスにわたって構築)に
  匹敵する規模の開発が必要であり、1パスでは実装していません。

`RjsonBackend`は`DbBackend` traitを実装しているため、将来的に
永続化・分散化されたRJSON専用エンジンに差し替える際も、呼び出し側
(ハンドラ等)のコード変更は不要な設計になっています。

## デメリットへの対応方針(ご指摘の4点)

1. **独自パース処理の限界(RDBへの正規化の難しさ)**: `RjsonBackend`は
   RDBではなくKVストア形状(`(table,key)→value`)のため、正規化の
   問題自体が今のところ発生しません。将来的なテーブル横断クエリ・
   結合が必要になった時点で再検討。
2. **ロックイン(特定DBへの依存)**: RJSON自体は`serde_json::Value`を
   値モデルとするため、PostgreSQLのJSONB型固有の機能には一切依存
   していません(`open-runo-rjson`クレートはDB非依存の純粋なパーサー
   /シリアライザ)。
3. **シリアライズのオーバーヘッド**: `to_string`は`serde_json`の
   コンパクトシリアライザをそのまま利用しており、RJSON独自のシリア
   ライズコストはほぼゼロ(パース時の追加コストのみ)。
4. **ORMエコシステムとの乖離**: このワークスペースは元々Diesel/SeaORM
   等の外部ORMに依存しない方針(`DbBackend` traitによる自前抽象化)
   のため、この問題は構造的に発生しません——他言語/フレームワークから
   はREST API経由でアクセスする設計であり、RJSON独自型をORMのRust
   マクロに認識させる必要がありません。
