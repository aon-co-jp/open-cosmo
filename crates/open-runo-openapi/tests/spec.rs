use open_runo_openapi::{docs_html, DocsUi, Operation, Spec, TsClient};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, JsonSchema)]
struct Item {
    id: String,
    count: u32,
    note: Option<String>,
    tags: Vec<String>,
    kind: Kind,
}

#[derive(Serialize, Deserialize, JsonSchema)]
enum Kind {
    A,
    B,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct NewItem {
    /// 表示名
    name: String,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct ListQuery {
    /// 取得件数
    limit: Option<u32>,
    prefix: String,
}

fn spec() -> Spec {
    Spec::new("demo", "1.0.0")
        .api_key_header("X-Api-Key")
        .op(Operation::get("/health")
            .summary("health")
            .public()
            .response_empty(200, "OK"))
        .op(Operation::post("/items")
            .summary("create")
            .tag("items")
            .body::<NewItem>()
            .ok::<Item>("created"))
        .op(Operation::get("/items")
            .summary("list")
            .query::<ListQuery>()
            .ok::<Vec<Item>>("all"))
        .op(Operation::get("/items/:id")
            .summary("get")
            .ok::<Item>("one")
            .response_empty(404, "none"))
        .op(Operation::delete("/items/{id}")
            .summary("del")
            .response_empty(204, "deleted"))
}

#[test]
fn builds_valid_openapi_with_refs_and_params() {
    let d = spec().build();
    assert_eq!(d["openapi"], "3.0.3");
    assert!(d["paths"]["/items/{id}"]["get"]["parameters"][0]["in"] == "path");
    assert_eq!(
        d["paths"]["/items/{id}"]["get"]["operationId"],
        "getItemsByid".replace("Byid", "ById")
    );
    assert_eq!(
        d["paths"]["/items"]["post"]["requestBody"]["content"]["application/json"]["schema"]
            ["$ref"],
        "#/components/schemas/NewItem"
    );
    // Vec<Item> はインライン配列で、中身はItemへの参照。
    assert_eq!(
        d["paths"]["/items"]["get"]["responses"]["200"]["content"]["application/json"]["schema"]
            ["items"]["$ref"],
        "#/components/schemas/Item"
    );
    // クエリ構造体が展開され、必須/任意が反映される。
    let params = d["paths"]["/items"]["get"]["parameters"]
        .as_array()
        .unwrap();
    let prefix = params.iter().find(|p| p["name"] == "prefix").unwrap();
    assert_eq!(prefix["required"], true);
    let limit = params.iter().find(|p| p["name"] == "limit").unwrap();
    assert_eq!(limit["required"], false);
    // enum・Option(nullable)がcomponentsに入る。
    assert!(d["components"]["schemas"]["Kind"].is_object());
    assert_eq!(
        d["components"]["schemas"]["Item"]["properties"]["note"]["nullable"],
        true
    );
    // 公開操作にはsecurity無し、他は必須。
    assert_eq!(
        d["paths"]["/health"]["get"]["security"],
        serde_json::json!([])
    );
    assert_eq!(
        d["paths"]["/items"]["get"]["security"][0]["ApiKeyAuth"],
        serde_json::json!([])
    );
    assert!(!d.to_string().contains("$defs"));
    assert!(!d.to_string().contains("\"null\""));
}

#[test]
fn lint_finds_problems() {
    assert!(spec().lint().is_empty(), "{:?}", spec().lint());
    let bad = Spec::new("x", "1")
        .op(Operation::get("/a"))
        .op(Operation::get("/a")
            .summary("dup")
            .response_empty(200, "ok"));
    let p = bad.lint();
    assert!(p.iter().any(|m| m.contains("重複したルート")));
    assert!(p.iter().any(|m| m.contains("レスポンス未定義")));
    assert!(p.iter().any(|m| m.contains("summary未設定")));
}

#[test]
fn undocumented_routes_are_reported_in_either_param_style() {
    let s = spec();
    let missing = s.undocumented([
        ("GET", "/items/:id"),
        ("POST", "/nope"),
        ("get", "/items/{id}"),
    ]);
    assert_eq!(missing, vec!["POST /nope".to_string()]);
}

#[test]
fn graphql_sdl_for_cosmo() {
    let sdl = spec().to_graphql_sdl();
    assert!(sdl.contains("scalar JSON"));
    assert!(sdl.contains("type Item {"));
    assert!(sdl.contains("input NewItemInput {"));
    assert!(sdl.contains("note: String\n"), "{sdl}");
    assert!(sdl.contains("count: Int!"));
    assert!(sdl.contains("tags: [String!]!"), "{sdl}");
    assert!(sdl.contains("enum Kind { A B }"), "{sdl}");
    assert!(sdl.contains("type Query {"));
    assert!(sdl.contains("getItemsById(id: String!): Item"), "{sdl}");
    assert!(sdl.contains("type Mutation {"));
    assert!(
        sdl.contains("postItems(input: NewItemInput!): Item"),
        "{sdl}"
    );
}

#[test]
fn tauri_manifest_and_typescript() {
    let m = spec().to_tauri_manifest();
    let names: Vec<&str> = m["commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"post_items") && names.contains(&"get_items_by_id"),
        "{names:?}"
    );
    let inv = spec().to_typescript(TsClient::TauriInvoke);
    assert!(
        inv.contains("invoke<Item>('get_items_by_id', args)"),
        "{inv}"
    );
    assert!(inv.contains("export type Item ="));
    let f = spec().to_typescript(TsClient::Fetch);
    assert!(f.contains("encodeURIComponent(args.id)"), "{f}");
    assert!(f.contains("call('POST', `/items`"), "{f}");
}

#[test]
fn docs_html_escapes_and_embeds_url() {
    let h = docs_html(DocsUi::SwaggerUi, "a<b", "/api/openapi.json");
    assert!(h.contains("a&lt;b") && h.contains("\"/api/openapi.json\""));
    assert!(docs_html(DocsUi::Redoc, "t", "/x").contains("Redoc.init"));
}

#[test]
fn root_path_gets_a_readable_operation_id() {
    let s = Spec::new("r", "1").op(Operation::get("/").summary("top").response_empty(200, "ok"));
    assert_eq!(s.operations()[0].operation_id(), "getRoot");
    assert!(s.to_graphql_sdl().contains("getRoot: JSON"));
}

#[test]
fn text_responses_are_declared_as_text_not_json() {
    let s = Spec::new("t", "1").op(Operation::post("/ok")
        .summary("plain ok")
        .public()
        .response_text(200, "ok", "text/plain"));
    let d = s.build();
    assert_eq!(
        d["paths"]["/ok"]["post"]["responses"]["200"]["content"]["text/plain"]["schema"]["type"],
        "string"
    );
    assert!(d["paths"]["/ok"]["post"]["responses"]["200"]["content"]["application/json"].is_null());
    assert!(s.to_graphql_sdl().contains("postOk: String"));
    assert!(s.to_typescript(TsClient::Fetch).contains("Promise<string>"));
}
