//! Poem SSR統合(HYBRID_NETWORK_ARCHITECTURE.md §0.9.3 Phase 3)。
//!
//! `open-runo-view::ssr::render_page` はフレームワーク非依存の
//! `String` を返すだけの関数なので、本モジュールは「Poemハンドラが
//! その`String`を`text/html`で返す」という薄い層に徹する
//! (open-runo-view自体にpoem依存を持ち込まない設計、crateドキュメント
//! 参照)。
//!
//! ここでは open-easyweb の実戦投入(`view_bridge::status_panel`)と
//! 同一のコンポーネント定義を使う代わりに、gateway側で自己完結する
//! デモ用コンポーネントを用意する(open-easywebはwasm32専用crateであり
//! ネイティブのPoemサーバから直接依存できないため、コンポーネント定義は
//! 両側で複製し、状態の型(JSON構造)だけを共有する契約とする——
//! §0.5の「中心技術だけ同期を取る」ミラー規則と同種の対応)。

use open_runo_view::hooks::Ctx;
use open_runo_view::ssr::{render_page, SsrPage};
use open_runo_view::{h, VNode};
use poem::{handler, web::Html, IntoResponse, Route};
use serde::{Deserialize, Serialize};

/// open-easyweb `view_bridge::StatusPanelState` と同一形状(§0.5ミラー契約)。
/// hydration時、クライアント側はこのJSONをそのままデシリアライズする。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StatusPanelState {
    pub site_name: String,
    pub domains: Vec<String>,
    pub healthy: bool,
}

/// open-easyweb `view_bridge::status_panel` と同一定義(§0.5参照)。
fn status_panel(ctx: &mut Ctx, props: &StatusPanelState) -> VNode {
    let (expanded, set_expanded) = ctx.use_state(|| false);
    let toggle_id = ctx.use_handler(move || set_expanded.update(|v| !v));

    let mut root = h("section")
        .attr("class", if props.healthy { "status ok" } else { "status ng" })
        .attr("data-openruno", "status-panel")
        .child(h("h2").child(props.site_name.as_str()).build())
        .child(
            h("p")
                .child(if props.healthy { "稼働中" } else { "停止中" })
                .build(),
        )
        .child(
            h("button")
                .attr("type", "button")
                .on("click", toggle_id)
                .child(if expanded { "詳細を隠す" } else { "詳細を表示" })
                .build(),
        );
    if expanded {
        root = root.child(
            h("ul")
                .children(
                    props
                        .domains
                        .iter()
                        .map(|d| h("li").key(d).child(d.as_str()).build()),
                )
                .build(),
        );
    }
    root.build()
}

/// `GET /ssr/status` — サーバサイドで初回レンダリングした完全なHTMLページを返す。
/// クライアント側(open-easyweb `openruno_hydrate`)は
/// `window.__OPEN_RUNO_STATE__` を読んで同じ `status_panel` をhydrateする。
#[handler]
fn status_page() -> impl IntoResponse {
    // 実運用ではTenantRegistry/DB由来の実データに差し替える(デモ値)。
    let props = StatusPanelState {
        site_name: "runo.tokyo".into(),
        domains: vec!["runo.tokyo".into(), "audiocafe.tokyo".into()],
        healthy: true,
    };
    let mut rt = open_runo_view::hooks::Runtime::new(status_panel);
    rt.rerender(&props);
    let body = rt.tree().cloned().unwrap_or_else(|| h("div").build());

    let hydration_json = serde_json::to_string(&props).unwrap_or_else(|_| "{}".into());
    let html = render_page(&SsrPage {
        title: &props.site_name,
        body: &body,
        root_id: "open-runo-root",
        hydration_json: Some(&hydration_json),
        head_extra: "",
        // open-easyweb側のwasmバンドルを読み込み、hydrationを起動する想定
        // (実パスはデプロイ構成に応じて調整。ここではプレースホルダ)。
        scripts: r#"<script type="module">
import init, { openruno_hydrate } from "/static/open_easyweb.js";
await init();
openruno_hydrate("open-runo-root");
</script>"#,
    });
    Html(html)
}

/// `/ssr` 配下のルートをまとめて返す。呼び出し側で
/// `Route::new().nest("/ssr", ssr::ssr_route())` のように組み込む。
pub fn ssr_route() -> Route {
    Route::new().at("/status", poem::get(status_page))
}

#[cfg(test)]
mod tests {
    use super::*;
    use poem::test::TestClient;

    #[tokio::test]
    async fn ssr_status_page_renders_expected_markup_and_hydration_state() {
        let cli = TestClient::new(ssr_route());
        let resp = cli.get("/status").send().await;
        resp.assert_status_is_ok();
        let body = resp.0.into_body().into_string().await.unwrap();

        assert!(body.starts_with("<!DOCTYPE html>"));
        assert!(body.contains("<title>runo.tokyo</title>"));
        assert!(body.contains("id=\"open-runo-root\""));
        assert!(body.contains("data-openruno=\"status-panel\""));
        assert!(body.contains("data-orv-click=\""), "button must carry the delegated-click marker");
        assert!(body.contains("window.__OPEN_RUNO_STATE__ = {"));
        assert!(body.contains("\"site_name\":\"runo.tokyo\""));
        assert!(body.contains("audiocafe.tokyo"));
        assert!(body.contains("openruno_hydrate(\"open-runo-root\")"));
    }
}
