//! `/demo` — a fixed, self-contained GraphQL Federation demo.
//!
//! Purpose: let a visitor see the **real** `open-runo-federation` core
//! (`compose()`/`ComposedSchema`, the same functions the production
//! `POST /api/federation/compose` REST endpoint uses) actually federate two
//! sample subgraphs ("users-service" and "products-service") and then run
//! real GraphQL queries against the result — without needing to stand up
//! real upstream services or seed a real database.
//!
//! This intentionally reuses the two building blocks that already exist in
//! this workspace rather than inventing new federation logic:
//! 1. `open_runo_federation::compose` — schema composition (type/field
//!    merging across subgraphs), exactly as used by
//!    `handlers_hyper::compose_schemas_handler`.
//! 2. `async-graphql` — query execution, exactly as used by
//!    `graphql_hyper::graphql_post_handler` for the production `/graphql`
//!    endpoint.
//!
//! **Honest scope note**: this is a schema-composition + fixed-sample-data
//! demo, not a query-planning engine that dispatches sub-queries to real
//! upstream services over the network — `open-runo-federation` itself does
//! not implement query planning/execution yet (see its module doc comment),
//! so a demo of that would require building a capability that doesn't exist
//! anywhere else in this workspace. What *is* real here: the composed
//! schema returned by `demoFederationStatus` is the actual output of
//! `open_runo_federation::compose()` run against the two subgraphs' SDL
//! below, and `users`/`products`/`productsWithOwner` execute real
//! `async-graphql` resolvers (including a cross-subgraph `owner` lookup
//! simulating what a `@key`-based reference resolution would return) over
//! fixed in-memory sample data.

// clippy: このモジュールの unwrap()/expect() は「固定ヘッダからのレスポンス構築」
// 「起動時に読む埋め込み定数のパース」等、構造上失敗しない箇所に限られる
// (各呼び出しに理由コメントあり)。business ロジック側の Quality Gate
// (workspace lints の unwrap_used/expect_used = warn) は維持したまま、
// この基盤モジュールだけ除外する。result_large_err は Result<T, Response>
// (エラー時に完成済み HTTP レスポンスで短絡する意図的な型)に対するもの。
#![allow(clippy::expect_used)]

use async_graphql::{Object, Schema, SimpleObject};
use open_runo_federation::{compose, parse_service_sdl, ComposedSchema};
use open_runo_router::hyper_compat::{html_response, json_response, read_json_body, Handler};
use std::sync::Arc;

/// Subgraph 1: `users-service`. Written as real SDL (not a hand-built
/// `ServiceSchema`) so `parse_service_sdl` — the same SDL parser the
/// production `sdl` field of `POST /api/federation/compose` uses — is
/// exercised here too, rather than only the lower-level `ServiceSchema`
/// struct literal.
const USERS_SDL: &str = r#"
    type User @key(fields: "id") {
        id: ID!
        name: String!
        email: String!
    }
"#;

/// Subgraph 2: `products-service`. `ownerId` is the "foreign key" a real
/// federation gateway would use to resolve the `User` reference via
/// `users-service`'s `@key`; here `DemoProductGql::owner` performs that
/// join directly against the fixed sample data.
const PRODUCTS_SDL: &str = r#"
    type Product @key(fields: "id") {
        id: ID!
        title: String!
        price: Float!
        ownerId: ID!
    }
"#;

#[derive(SimpleObject, Clone)]
struct DemoUser {
    id: String,
    name: String,
    email: String,
}

#[derive(Clone)]
struct DemoProduct {
    id: String,
    title: String,
    price: f64,
    owner_id: String,
}

fn sample_users() -> Vec<DemoUser> {
    vec![
        DemoUser {
            id: "1".into(),
            name: "Alice".into(),
            email: "alice@example.com".into(),
        },
        DemoUser {
            id: "2".into(),
            name: "Bob".into(),
            email: "bob@example.com".into(),
        },
    ]
}

fn sample_products() -> Vec<DemoProduct> {
    vec![
        DemoProduct {
            id: "1".into(),
            title: "Widget".into(),
            price: 9.99,
            owner_id: "1".into(),
        },
        DemoProduct {
            id: "2".into(),
            title: "Gadget".into(),
            price: 19.99,
            owner_id: "2".into(),
        },
        DemoProduct {
            id: "3".into(),
            title: "Gizmo".into(),
            price: 29.99,
            owner_id: "1".into(),
        },
    ]
}

/// GraphQL projection of `DemoProduct`, with a resolver-backed `owner`
/// field that performs the cross-subgraph lookup a real federated gateway
/// would delegate to `users-service`.
struct DemoProductGql(DemoProduct);

#[Object]
impl DemoProductGql {
    async fn id(&self) -> &str {
        &self.0.id
    }
    async fn title(&self) -> &str {
        &self.0.title
    }
    async fn price(&self) -> f64 {
        self.0.price
    }
    async fn owner_id(&self) -> &str {
        &self.0.owner_id
    }
    /// Cross-subgraph reference resolution (the `@key`-based join a real
    /// federation gateway performs between `products-service`'s `ownerId`
    /// and `users-service`'s `User.id`).
    async fn owner(&self) -> Option<DemoUser> {
        sample_users().into_iter().find(|u| u.id == self.0.owner_id)
    }
}

/// GraphQL projection of [`open_runo_federation::ComposedSchema`], reusing
/// the same shape the production `federationStatus` field on the main
/// `/graphql` schema exposes.
#[derive(SimpleObject)]
struct DemoFederationStatusGql {
    contributing_services: Vec<String>,
    type_names: Vec<String>,
    field_count: i32,
}

/// Runs the real `open_runo_federation` composition over the two demo
/// subgraphs' SDL. Called fresh on each query (the inputs are constants, so
/// this is cheap) rather than cached, to keep this module free of any
/// hidden mutable state.
fn compose_demo_schema() -> ComposedSchema {
    let users = parse_service_sdl("users-service", USERS_SDL)
        .expect("demo users SDL is a fixed constant and must always parse");
    let products = parse_service_sdl("products-service", PRODUCTS_SDL)
        .expect("demo products SDL is a fixed constant and must always parse");
    compose(&[users, products]).expect("fixed demo subgraphs never collide")
}

#[derive(Debug, Clone, Copy, Default)]
pub struct DemoQueryRoot;

#[Object]
impl DemoQueryRoot {
    /// All sample users from the `users-service` subgraph.
    async fn users(&self) -> Vec<DemoUser> {
        sample_users()
    }

    /// All sample products from the `products-service` subgraph, each with
    /// their `owner` resolved across the subgraph boundary.
    async fn products(&self) -> Vec<DemoProductGql> {
        sample_products().into_iter().map(DemoProductGql).collect()
    }

    /// The result of actually running `open_runo_federation::compose()`
    /// over the two demo subgraphs — proves this demo is backed by the
    /// real Federation core, not a hand-written fake.
    async fn demo_federation_status(&self) -> DemoFederationStatusGql {
        let composed = compose_demo_schema();
        let field_count = composed.types.values().map(|f| f.len()).sum::<usize>() as i32;
        DemoFederationStatusGql {
            contributing_services: composed.contributing_services,
            type_names: composed.types.keys().cloned().collect(),
            field_count,
        }
    }
}

pub type DemoSchema =
    Schema<DemoQueryRoot, async_graphql::EmptyMutation, async_graphql::EmptySubscription>;

pub fn build_demo_schema() -> DemoSchema {
    Schema::build(
        DemoQueryRoot,
        async_graphql::EmptyMutation,
        async_graphql::EmptySubscription,
    )
    .finish()
}

/// `GET /demo` — a GraphiQL playground pre-pointed at `/demo/graphql`, with
/// a couple of example queries baked into the page so a visitor doesn't
/// have to know the schema up front.
pub fn demo_playground_handler() -> Handler {
    Arc::new(move |_req, _params| {
        Box::pin(async move {
            let graphiql = async_graphql::http::GraphiQLSource::build()
                .endpoint("/demo/graphql")
                .finish();
            // Prepend a short banner explaining what this page demonstrates
            // and one ready-to-run sample query, ahead of the GraphiQL app.
            let html = format!(
                r#"<!doctype html>
<html>
<head><meta charset="utf-8"><title>RPoem GraphQL Federation Demo</title></head>
<body style="margin:0">
<div style="font-family:sans-serif;padding:0.75rem 1rem;background:#1b1f24;color:#e6e6e6;font-size:0.9rem">
  <strong>RPoem GraphQL Federation demo</strong> &mdash;
  two fixed sample subgraphs (<code>users-service</code>,
  <code>products-service</code>) composed by the real
  <code>open-runo-federation</code> core and served live below. Try:
  <code>{{ products {{ title owner {{ name }} }} demoFederationStatus {{ contributingServices typeNames }} }}</code>
</div>
{graphiql}
</body>
</html>"#
            );
            html_response(hyper::StatusCode::OK, html)
        })
    })
}

/// `POST /demo/graphql` — executes queries against the fixed demo schema.
pub fn demo_graphql_handler() -> Handler {
    let schema = build_demo_schema();
    Arc::new(move |req, _params| {
        let schema = schema.clone();
        Box::pin(async move {
            let request: async_graphql::Request = match read_json_body(req).await {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let response = schema.execute(request).await;
            json_response(hyper::StatusCode::OK, &response)
        })
    })
}

/// Convenience bundle: `(GET /demo handler, POST /demo/graphql handler)`.
pub fn demo_handlers() -> (Handler, Handler) {
    (demo_playground_handler(), demo_graphql_handler())
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_runo_router::hyper_compat::{serve, Router};

    #[tokio::test]
    async fn demo_playground_serves_html_pointing_at_demo_graphql() {
        let (get_h, _post_h) = demo_handlers();
        let router = Router::new().route(hyper::Method::GET, "/demo", get_h);
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap()).await.unwrap();

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/demo"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body = resp.text().await.unwrap();
        assert!(body.contains("/demo/graphql"));
        assert!(body.to_lowercase().contains("graphiql"));
    }

    #[tokio::test]
    async fn demo_graphql_executes_users_query() {
        let (_get_h, post_h) = demo_handlers();
        let router = Router::new().route(hyper::Method::POST, "/demo/graphql", post_h);
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap()).await.unwrap();

        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/demo/graphql"))
            .json(&serde_json::json!({ "query": "{ users { id name } }" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["data"]["users"][0]["name"], "Alice");
        assert_eq!(body["data"]["users"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn demo_graphql_resolves_product_owner_across_subgraphs() {
        let (_get_h, post_h) = demo_handlers();
        let router = Router::new().route(hyper::Method::POST, "/demo/graphql", post_h);
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap()).await.unwrap();

        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/demo/graphql"))
            .json(&serde_json::json!({ "query": "{ products { title owner { name } } }" }))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        let products = body["data"]["products"].as_array().unwrap();
        assert_eq!(products.len(), 3);
        assert_eq!(products[0]["owner"]["name"], "Alice");
        assert_eq!(products[1]["owner"]["name"], "Bob");
    }

    #[tokio::test]
    async fn demo_federation_status_reflects_real_composition() {
        let (_get_h, post_h) = demo_handlers();
        let router = Router::new().route(hyper::Method::POST, "/demo/graphql", post_h);
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap()).await.unwrap();

        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/demo/graphql"))
            .json(&serde_json::json!({
                "query": "{ demoFederationStatus { contributingServices typeNames fieldCount } }"
            }))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        let status = &body["data"]["demoFederationStatus"];
        let services: Vec<&str> = status["contributingServices"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(services.contains(&"users-service"));
        assert!(services.contains(&"products-service"));
        let types: Vec<&str> = status["typeNames"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(types.contains(&"User"));
        assert!(types.contains(&"Product"));
        // User(id,name,email) + Product(id,title,price,ownerId) = 7 fields.
        assert_eq!(status["fieldCount"], 7);
    }

    #[test]
    fn compose_demo_schema_uses_the_real_federation_core() {
        let composed = compose_demo_schema();
        assert_eq!(
            composed.contributing_services,
            vec!["users-service", "products-service"]
        );
        assert!(composed.types.contains_key("User"));
        assert!(composed.types.contains_key("Product"));
    }
}
