//! `proto/open_runo/admin/v1/admin.proto` から prost のメッセージ型と記述子セットを生成する。
//! `protox`(純Rustのprotobufコンパイラ)を使うため、`protoc`のインストールは要らない。

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=proto");
    let fds = protox::compile(["open_runo/admin/v1/admin.proto"], ["proto"])
        .expect("admin.proto must compile");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    // リフレクション用の記述子セット(依存の google/protobuf/struct.proto を含む)。
    std::fs::write(
        out.join("admin_fds.bin"),
        prost::Message::encode_to_vec(&fds),
    )
    .expect("write descriptor set");
    prost_build::Config::new()
        .compile_fds(fds)
        .expect("prost codegen");
}
