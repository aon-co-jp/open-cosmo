//! 外から来る JSON を読む共通の入口(方針: RS-JSON の安全モード + SIMD の sonic-rs)。
//!
//! 先に `rust_json::parse_secure_sonic` で「サイズ・深さの上限と重複キーの拒否」を検査してから、
//! 型へ読む(`sonic_rs`)。HTTP 本文・WebSocket メッセージ・リモートのサブグラフの応答など、
//! 信頼できない入力はここを通す。自分で書いた設定やローカルのファイルは `sonic_rs::from_str` でよい。

use serde::de::DeserializeOwned;

/// 既定の上限: 16 MiB・深さ64・重複キーは拒否。本文の大きさそのものは別途 `body_limit` でも制限される。
pub fn limits() -> rust_json::Limits {
    rust_json::Limits { max_bytes: 16 * 1024 * 1024, max_depth: 64, reject_duplicate_keys: true }
}

/// 上限と重複キーを検査する(値は捨てる)。
pub fn validate(text: &str) -> Result<(), String> {
    rust_json::parse_secure_sonic(text, &limits()).map(|_| ()).map_err(|e| e.to_string())
}

/// 検査してから型 `T` へ読む。
pub fn from_str<T: DeserializeOwned>(text: &str) -> Result<T, String> {
    validate(text)?;
    sonic_rs::from_str(text).map_err(|e| e.to_string())
}

/// 検査してから型 `T` へ読む(UTF-8 でなければエラー)。
pub fn from_slice<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "body is not valid UTF-8".to_string())?;
    from_str(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_valid_json_and_rejects_duplicates_and_deep_nesting() {
        let v: serde_json::Value = from_str(r#"{"a":[1,2,{"b":null}]}"#).unwrap();
        assert_eq!(v["a"][0], 1);
        assert!(from_str::<serde_json::Value>(r#"{"a":1,"a":2}"#).unwrap_err().contains("duplicate"));
        let deep = format!("{}{}", "[".repeat(100), "]".repeat(100));
        assert!(from_str::<serde_json::Value>(&deep).is_err());
        assert!(from_slice::<serde_json::Value>(&[0xff, 0xfe]).is_err());
        assert!(from_str::<serde_json::Value>("{x").is_err());
    }
}
