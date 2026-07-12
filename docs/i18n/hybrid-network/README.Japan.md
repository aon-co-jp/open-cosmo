# ハイブリッドネットワーク・アーキテクチャ(要約)


**使命(v0.2で統合):** 絶対に紛失してはいけないデータの、確実な配信と確実な読み書き — 3Dオンラインゲームの課金アイテム、オンライン金融、オンライン証券が対象。速度や4層通信は、この使命に奉仕するためにあり、これと競合してはならない。

**目標:** 4層の通信スタック(生UDP → QUIC/HTTP3 → TCPフォールバック → GraphQL federation多重化)と、`aruaru-db` のACID保証、`open-raid-z` のZFS互換の整合性を、`open-runo`・`poem-cosmo-tauri`・`open-web-server`・`aruaru-db`・`open-raid-z` の各プロジェクト間で組み合わせる。

**現状:** `aruaru-db` のPoem統合は高速であることを確認済み。`open-runo` とのSQL UPSERT互換性はまだ未解決。`open-raid-z` は非アライメントI/Oとマイグレーション機能が動作するが、LinuxのCI環境ではWindows固有の型が使用不可。`open-web-server` は未監査。

**次の一手:** (1) UPSERTパーサーの修正、(2) `open-web-server` の監査、(3) 共有の通信ネゴシエーション仕様策定、(4) DB書き込みパスへのZFS方式チェックサム統合、(5) QUIC/UDP高速パスの実装は最後に。

詳細は `docs/HYBRID_NETWORK_ARCHITECTURE.md` を参照。注記: リアルタイムのWeb調査なしで作成されているため、「最先端」に関する主張はベンチマーク実施前は未検証として扱うこと。
