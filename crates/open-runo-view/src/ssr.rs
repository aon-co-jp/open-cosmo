//! Phase 3 — SSRページ生成と hydration ペイロード。
//!
//! フレームワーク非依存のHTML完全ページ生成器。Poem統合は「このcrateに
//! poem依存を持ち込む」のではなく、**Poemハンドラがこの関数の戻り値
//! (`String`)を `text/html` で返すだけ**という薄い形にする(sandbox検証
//! 可能性と、axum等の他フレームワークでも同一APIで使える汎用性のため)。
//!
//! Poem側の典型形(open-runo / poem-cosmo-tauri の gateway に置く):
//! ```ignore
//! #[handler]
//! fn page() -> Html<String> {
//!     let body = my_component(&props);
//!     Html(ssr::render_page(&SsrPage {
//!         title: "ショップ",
//!         body: &body,
//!         hydration_json: Some(&serde_json::to_string(&props)?),
//!         ..SsrPage::default_for(&body)
//!     }))
//! }
//! ```
//! クライアント(wasm)側は `hydration_data()` 相当として
//! `window.__OPEN_RUNO_STATE__` を読み、同じコンポーネントを
//! `DomMount::attach("open-runo-root")` にマウントする。

use crate::{render_html, VNode};

/// フルページSSRの入力。
pub struct SsrPage<'a> {
    /// `<title>`(HTMLエスケープされる)。
    pub title: &'a str,
    /// ボディに描画する仮想ツリー。
    pub body: &'a VNode,
    /// マウントポイントのid(クライアント側 `DomMount::attach` と一致させる)。
    pub root_id: &'a str,
    /// hydration用の初期状態JSON。`<script>` に
    /// `window.__OPEN_RUNO_STATE__ = ...` として埋め込まれる。
    /// **`</script>`注入対策のエスケープは本関数側で行う**ので生JSONを渡してよい。
    pub hydration_json: Option<&'a str>,
    /// 追加の`<head>`内HTML(CSS link等)。エスケープされない(信頼済み入力専用)。
    pub head_extra: &'a str,
    /// wasmバンドルのJS読み込みタグ等。エスケープされない(信頼済み入力専用)。
    pub scripts: &'a str,
}

impl<'a> SsrPage<'a> {
    pub fn default_for(body: &'a VNode) -> Self {
        Self {
            title: "",
            body,
            root_id: "open-runo-root",
            hydration_json: None,
            head_extra: "",
            scripts: "",
        }
    }
}

fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

/// `<script>`内へJSONを安全に埋め込むためのエスケープ。
/// `</script>` 早期終了と `<!--` によるHTMLコメント攻撃を防ぐ
/// (`<` を `\u003c` に置換する標準的手法)。
fn escape_json_for_script(json: &str) -> String {
    json.replace('<', "\\u003c")
}

/// 完全なHTML文書を生成する(ReactDOMServerでのページ組み立てに相当)。
pub fn render_page(page: &SsrPage) -> String {
    let mut s = String::with_capacity(1024);
    s.push_str("<!DOCTYPE html>\n<html>\n<head>\n<meta charset=\"utf-8\" />\n");
    s.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\" />\n");
    s.push_str("<title>");
    s.push_str(&escape_text(page.title));
    s.push_str("</title>\n");
    s.push_str(page.head_extra);
    s.push_str("</head>\n<body>\n<div id=\"");
    // root_id は属性値: 二重引用符をエスケープ
    s.push_str(&page.root_id.replace('"', "&quot;"));
    s.push_str("\">");
    s.push_str(&render_html(page.body));
    s.push_str("</div>\n");
    if let Some(j) = page.hydration_json {
        s.push_str("<script>window.__OPEN_RUNO_STATE__ = ");
        s.push_str(&escape_json_for_script(j));
        s.push_str(";</script>\n");
    }
    s.push_str(page.scripts);
    s.push_str("\n</body>\n</html>\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::h;

    #[test]
    fn renders_full_page_with_body_and_title_escaped() {
        let body = h("main").child("こんにちは").build();
        let html = render_page(&SsrPage {
            title: "A<B>&C",
            ..SsrPage::default_for(&body)
        });
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(html.contains("<title>A&lt;B&gt;&amp;C</title>"));
        assert!(html.contains("<div id=\"open-runo-root\"><main>こんにちは</main></div>"));
        assert!(!html.contains("__OPEN_RUNO_STATE__"));
    }

    #[test]
    fn hydration_json_is_script_injection_safe() {
        let body = h("div").build();
        let evil = r#"{"x":"</script><script>alert(1)</script>"}"#;
        let html = render_page(&SsrPage {
            hydration_json: Some(evil),
            ..SsrPage::default_for(&body)
        });
        assert!(
            !html.contains("</script><script>alert"),
            "raw </script> must not survive: {html}"
        );
        assert!(html.contains(r#"\u003c/script>"#), "escaped form must be present: {html}");
        assert!(html.contains("window.__OPEN_RUNO_STATE__ = {"));
    }

    #[test]
    fn ssr_output_matches_what_client_would_mount() {
        // SSRのdiv内HTMLは、クライアントが同じVNodeをマウントした結果と
        // 同一でなければならない(hydration一致性の基礎)。
        let body = h("ul")
            .child(h("li").key("a").attr("class", "x").child("A").build())
            .build();
        let html = render_page(&SsrPage::default_for(&body));
        let inner = crate::render_html(&body);
        assert!(html.contains(&format!("<div id=\"open-runo-root\">{inner}</div>")));
    }
}
