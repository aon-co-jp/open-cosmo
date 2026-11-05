//! Remote subgraph forwarding for `POST /graphql` (2026-10-18).
//!
//! Until now the gateway could only compose subgraph *schemas*
//! (`open-runo-federation`) and execute against its own in-process
//! schema; it could not send a query to an external subgraph over HTTP.
//! This module adds that: remote subgraphs are registered by name + URL,
//! their root `Query`/`Mutation` field names are discovered through
//! introspection, and a request whose root fields all belong to one remote
//! subgraph is forwarded to it unchanged (the response is returned as is).
//!
//! **Scope (honest)**: this is root-field routing, not a federated query
//! planner. A request that mixes root fields of different subgraphs (or of
//! a remote subgraph and the local schema) is rejected with a GraphQL
//! error instead of being split and merged. Entity resolution across
//! subgraphs (`@key` joins) is not implemented.
//!
//! Configuration: `OPEN_RUNO_SUBGRAPHS="name=http://host:port/graphql,..."`.

use async_graphql::parser::{
    parse_query,
    types::{DocumentOperations, ExecutableDocument, Selection, SelectionSet},
};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

const INTROSPECTION: &str =
    "{ __schema { queryType { fields { name } } mutationType { fields { name } } } }";
const REDISCOVER_AFTER: Duration = Duration::from_secs(5);

#[derive(Debug)]
struct Subgraph {
    name: String,
    url: String,
    fields: HashSet<String>,
    last_attempt: Option<Instant>,
}

/// Registry of remote subgraphs; cheap to clone (shared state).
#[derive(Clone, Debug)]
pub struct RemoteSubgraphs {
    inner: Arc<Mutex<Vec<Subgraph>>>,
    client: reqwest::Client,
}

#[derive(Debug)]
pub enum Routed {
    /// No root field belongs to a remote subgraph: execute locally.
    Local,
    /// Forwarded; this is the subgraph's JSON response body.
    Forwarded(serde_json::Value),
    /// Rejected with a GraphQL error message.
    Rejected(String),
}

impl RemoteSubgraphs {
    pub fn new(entries: Vec<(String, String)>) -> Self {
        let subgraphs = entries
            .into_iter()
            .map(|(name, url)| Subgraph {
                name,
                url,
                fields: HashSet::new(),
                last_attempt: None,
            })
            .collect();
        Self {
            inner: Arc::new(Mutex::new(subgraphs)),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
        }
    }

    /// `OPEN_RUNO_SUBGRAPHS="items=http://localhost:4001/graphql,users=http://..."`
    pub fn from_env() -> Self {
        Self::new(parse_env(
            &std::env::var("OPEN_RUNO_SUBGRAPHS").unwrap_or_default(),
        ))
    }

    pub async fn is_empty(&self) -> bool {
        self.inner.lock().await.is_empty()
    }

    async fn discover(&self, sg: &mut Subgraph) {
        sg.last_attempt = Some(Instant::now());
        let resp = self
            .client
            .post(&sg.url)
            .json(&serde_json::json!({ "query": INTROSPECTION }))
            .send()
            .await;
        let Ok(resp) = resp else {
            tracing::warn!(subgraph = %sg.name, url = %sg.url, "subgraph introspection failed (unreachable)");
            return;
        };
        let Ok(v) = resp.json::<serde_json::Value>().await else {
            return;
        };
        let mut fields = HashSet::new();
        for root in ["queryType", "mutationType"] {
            if let Some(list) = v
                .pointer(&format!("/data/__schema/{root}/fields"))
                .and_then(|f| f.as_array())
            {
                for f in list {
                    if let Some(n) = f.get("name").and_then(|n| n.as_str()) {
                        fields.insert(n.to_string());
                    }
                }
            }
        }
        if !fields.is_empty() {
            tracing::info!(subgraph = %sg.name, fields = fields.len(), "subgraph root fields discovered");
            sg.fields = fields;
        }
    }

    /// Route one request. `variables` / `operation_name` are forwarded as given.
    pub async fn route(
        &self,
        query: &str,
        operation_name: Option<&str>,
        variables: serde_json::Value,
    ) -> Routed {
        let Ok(doc) = parse_query(query) else {
            return Routed::Local;
        };
        let roots = root_fields(&doc, operation_name);
        if roots.is_empty() {
            return Routed::Local;
        }

        let mut guard = self.inner.lock().await;
        // (Re)discover subgraphs that have no fields yet (e.g. started after the gateway).
        for i in 0..guard.len() {
            let stale = guard[i]
                .last_attempt
                .is_none_or(|t| t.elapsed() > REDISCOVER_AFTER);
            if guard[i].fields.is_empty() && stale {
                let mut sg = std::mem::replace(
                    &mut guard[i],
                    Subgraph {
                        name: String::new(),
                        url: String::new(),
                        fields: HashSet::new(),
                        last_attempt: None,
                    },
                );
                self.discover(&mut sg).await;
                guard[i] = sg;
            }
        }

        let mut target: Option<(String, String)> = None;
        let mut local = false;
        for f in &roots {
            match guard.iter().find(|s| s.fields.contains(f)) {
                Some(s) => match &target {
                    None => target = Some((s.name.clone(), s.url.clone())),
                    Some((n, _)) if *n == s.name => {}
                    Some((n, _)) => {
                        return Routed::Rejected(format!(
                            "query spans subgraphs `{n}` and `{}`; cross-subgraph queries are not supported by this gateway yet",
                            s.name
                        ))
                    }
                },
                None => local = true,
            }
        }
        drop(guard);

        let Some((name, url)) = target else {
            return Routed::Local;
        };
        if local {
            return Routed::Rejected(format!(
                "query mixes fields of subgraph `{name}` with local fields; cross-subgraph queries are not supported by this gateway yet"
            ));
        }
        let body = serde_json::json!({ "query": query, "variables": variables, "operationName": operation_name });
        match self.client.post(&url).json(&body).send().await {
            Ok(r) => match r.json::<serde_json::Value>().await {
                Ok(v) => Routed::Forwarded(v),
                Err(e) => Routed::Rejected(format!(
                    "subgraph `{name}` returned an invalid response: {e}"
                )),
            },
            Err(e) => Routed::Rejected(format!("subgraph `{name}` is unreachable: {e}")),
        }
    }
}

pub fn parse_env(s: &str) -> Vec<(String, String)> {
    s.split(',')
        .filter_map(|p| {
            let (n, u) = p.trim().split_once('=')?;
            let (n, u) = (n.trim(), u.trim());
            (!n.is_empty() && !u.is_empty()).then(|| (n.to_string(), u.to_string()))
        })
        .collect()
}

/// Root field names of the selected operation (introspection `__*` fields are skipped).
pub fn root_fields(doc: &ExecutableDocument, operation_name: Option<&str>) -> Vec<String> {
    let op = match &doc.operations {
        DocumentOperations::Single(op) => &op.node,
        DocumentOperations::Multiple(map) => match operation_name {
            Some(n) => match map.iter().find(|(k, _)| k.as_str() == n) {
                Some((_, op)) => &op.node,
                None => return vec![],
            },
            None => match map.values().next() {
                Some(op) if map.len() == 1 => &op.node,
                _ => return vec![],
            },
        },
    };
    let mut out = Vec::new();
    collect(doc, &op.selection_set.node, &mut out, 0);
    out
}

fn collect(doc: &ExecutableDocument, set: &SelectionSet, out: &mut Vec<String>, depth: usize) {
    if depth > 16 {
        return;
    }
    for sel in &set.items {
        match &sel.node {
            Selection::Field(f) => {
                let n = f.node.name.node.as_str();
                if !n.starts_with("__") {
                    out.push(n.to_string());
                }
            },
            Selection::InlineFragment(f) => {
                collect(doc, &f.node.selection_set.node, out, depth + 1)
            },
            Selection::FragmentSpread(s) => {
                if let Some(frag) = doc.fragments.get(&s.node.fragment_name.node) {
                    collect(doc, &frag.node.selection_set.node, out, depth + 1);
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots(q: &str, op: Option<&str>) -> Vec<String> {
        root_fields(&parse_query(q).unwrap(), op)
    }

    #[test]
    fn extracts_root_fields_incl_fragments_and_skips_introspection() {
        assert_eq!(roots("{ items { id } __typename }", None), vec!["items"]);
        assert_eq!(
            roots("mutation($n:String!){ createItem(name:$n){ id } }", None),
            vec!["createItem"]
        );
        assert_eq!(roots("query A { a } query B { b }", Some("B")), vec!["b"]);
        assert!(roots("query A { a } query B { b }", None).is_empty());
        assert_eq!(
            roots("{ ...F } fragment F on Query { items { id } }", None),
            vec!["items"]
        );
    }

    #[test]
    fn parses_env() {
        assert_eq!(
            parse_env("items=http://a:1/graphql, users = http://b:2/graphql,bad,=x"),
            vec![
                ("items".into(), "http://a:1/graphql".into()),
                ("users".into(), "http://b:2/graphql".into())
            ]
        );
    }

    #[tokio::test]
    async fn unreachable_subgraph_falls_back_to_local() {
        let r = RemoteSubgraphs::new(vec![("x".into(), "http://127.0.0.1:1/graphql".into())]);
        assert!(matches!(
            r.route("{ items { id } }", None, serde_json::json!({}))
                .await,
            Routed::Local
        ));
    }

    /// A fake subgraph served over real HTTP: introspection + one query/mutation.
    async fn fake_subgraph() -> String {
        fake_subgraph_with("items", "createItem").await
    }

    async fn fake_subgraph_with(query_field: &'static str, mutation_field: &'static str) -> String {
        use open_runo_router::hyper_compat::{
            json_response, read_json_body, serve, Handler, Router,
        };
        let h: Handler = Arc::new(move |req, _p| {
            Box::pin(async move {
                let body: serde_json::Value = read_json_body(req).await.unwrap_or_default();
                let q = body["query"].as_str().unwrap_or("");
                let out = if q.contains("__schema") {
                    serde_json::json!({"data":{"__schema":{"queryType":{"fields":[{"name":query_field}]},"mutationType":{"fields":[{"name":mutation_field}]}}}})
                } else {
                    serde_json::json!({"data":{"echo":q,"vars":body["variables"]}})
                };
                json_response(hyper::StatusCode::OK, &out)
            })
        });
        let router = Router::new().route(hyper::Method::POST, "/graphql", h);
        let (addr, handle) = serve(router, "127.0.0.1:0".parse().unwrap()).await.unwrap();
        std::mem::forget(handle);
        format!("http://{addr}/graphql")
    }

    #[tokio::test]
    async fn forwards_queries_and_mutations_with_variables() {
        let r = RemoteSubgraphs::new(vec![("items".into(), fake_subgraph().await)]);
        match r
            .route("{ items { id } }", None, serde_json::json!({}))
            .await
        {
            Routed::Forwarded(v) => assert_eq!(v["data"]["echo"], "{ items { id } }"),
            other => panic!("expected Forwarded, got {other:?}"),
        }
        match r
            .route(
                "mutation($n:String!){ createItem(name:$n){ id } }",
                None,
                serde_json::json!({"n":"x"}),
            )
            .await
        {
            Routed::Forwarded(v) => assert_eq!(v["data"]["vars"]["n"], "x"),
            other => panic!("expected Forwarded, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rejects_mixed_local_and_remote_roots_and_keeps_unknown_local() {
        let r = RemoteSubgraphs::new(vec![("items".into(), fake_subgraph().await)]);
        assert!(matches!(
            r.route(
                "{ items { id } schemas { id } }",
                None,
                serde_json::json!({})
            )
            .await,
            Routed::Rejected(_)
        ));
        assert!(matches!(
            r.route("{ schemas { id } }", None, serde_json::json!({}))
                .await,
            Routed::Local
        ));
    }

    #[tokio::test]
    async fn rejects_queries_spanning_two_remote_subgraphs() {
        let r = RemoteSubgraphs::new(vec![
            (
                "items".into(),
                fake_subgraph_with("items", "createItem").await,
            ),
            (
                "users".into(),
                fake_subgraph_with("users", "createUser").await,
            ),
        ]);
        assert!(matches!(
            r.route("{ items { id } users { id } }", None, serde_json::json!({}))
                .await,
            Routed::Rejected(_)
        ));
        assert!(matches!(
            r.route("{ users { id } }", None, serde_json::json!({}))
                .await,
            Routed::Forwarded(_)
        ));
    }
}
