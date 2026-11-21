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
//! **Scope (honest)**: this is root-field routing, not a full federated
//! query planner. A read-only query whose root fields belong to several
//! remote subgraphs is split into one sub-query per subgraph (concurrent,
//! `data`/`errors` merged; 2026-11-19). Mutations spanning subgraphs, a mix
//! of remote and local fields, and fragments/directives in a split query
//! are rejected with a GraphQL error. Entity resolution across subgraphs
//! (`@key` joins through `_entities`) works at any nesting depth; `@requires` /
//! `@provides` are not implemented.
//!
//! Configuration: `OPEN_RUNO_SUBGRAPHS="name=http://host:port/graphql,..."`.

use async_graphql::parser::{
    parse_query,
    types::{
        DocumentOperations, ExecutableDocument, Field, InlineFragment, OperationDefinition,
        OperationType, Selection, SelectionSet,
    },
};
use open_runo_federation::{parse_subgraph_model, SubgraphModel};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

const INTROSPECTION: &str =
    "{ __schema { queryType { name fields { name } } mutationType { name fields { name } } } }";
const REDISCOVER_AFTER: Duration = Duration::from_secs(5);

/// Request headers that may be forwarded to a remote subgraph. `x-reaxt` (a non-secret CSRF marker
/// that Reaxt subgraphs require on mutations) goes to every subgraph; `authorization` goes only to
/// a subgraph that is trusted right now (see `CREDENTIAL_HEADERS`). `cookie` is never forwarded.
pub const FORWARD_HEADERS: &[&str] = &["x-reaxt", "authorization"];

/// The subset of the headers above that carry credentials: forwarded only to trusted subgraphs.
pub const CREDENTIAL_HEADERS: &[&str] = &["authorization"];

#[derive(Debug)]
struct Subgraph {
    name: String,
    url: String,
    fields: HashSet<String>,
    /// Types, return types and `@key`s from the subgraph's `_service { sdl }` (if it answers).
    model: Option<SubgraphModel>,
    last_attempt: Option<Instant>,
}

/// Registry of remote subgraphs; cheap to clone (shared state).
#[derive(Clone, Debug)]
pub struct RemoteSubgraphs {
    inner: Arc<Mutex<Vec<Subgraph>>>,
    client: reqwest::Client,
    /// Names allowed to receive credentials (`OPEN_RUNO_TRUSTED_SUBGRAPHS`). Empty = nobody.
    trusted: Arc<HashSet<String>>,
    /// Subgraphs whose trust was revoked at run time, until the given instant.
    distrusted: Arc<std::sync::Mutex<HashMap<String, Instant>>>,
}

/// How long a misbehaving subgraph stays without credentials.
const DISTRUST_FOR: Duration = Duration::from_secs(60);

/// Credentials may only travel over a channel that cannot be read on the way:
/// `https`, or a loopback address (same machine).
pub fn is_safe_channel(url: &str) -> bool {
    if url.starts_with("https://") {
        return true;
    }
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
    let host = if let Some(inner) = host.strip_prefix('[') {
        inner.split(']').next().unwrap_or("")
    } else {
        host.rsplit_once(':').map_or(host, |(h, _)| h)
    };
    matches!(host, "localhost" | "127.0.0.1" | "::1")
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
                model: None,
                last_attempt: None,
            })
            .collect();
        Self {
            inner: Arc::new(Mutex::new(subgraphs)),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
            trusted: Arc::new(HashSet::new()),
            distrusted: Arc::new(std::sync::Mutex::new(HashMap::new())),
        }
    }

    /// Names of the subgraphs that may receive credential headers (hybrid trust: a name on this
    /// list is necessary but not sufficient — see [`credentials_allowed`](Self::credentials_allowed)).
    pub fn with_trusted<I: IntoIterator<Item = String>>(mut self, names: I) -> Self {
        self.trusted = Arc::new(names.into_iter().filter(|n| !n.is_empty()).collect());
        self
    }

    /// Whether `name` (reachable at `url`) may receive credentials right now:
    /// on the allow-list AND a safe channel AND not revoked in the last minute.
    pub fn credentials_allowed(&self, name: &str, url: &str) -> bool {
        if !self.trusted.contains(name) || !is_safe_channel(url) {
            return false;
        }
        let mut d = self.distrusted.lock().unwrap_or_else(|e| e.into_inner());
        match d.get(name) {
            Some(until) if *until > Instant::now() => false,
            Some(_) => {
                d.remove(name);
                true
            },
            None => true,
        }
    }

    fn revoke(&self, name: &str) {
        let mut d = self.distrusted.lock().unwrap_or_else(|e| e.into_inner());
        d.insert(name.to_string(), Instant::now() + DISTRUST_FOR);
        tracing::warn!(subgraph = %name, "subgraph misbehaved: credentials withheld for 60s");
    }

    /// `OPEN_RUNO_SUBGRAPHS="items=http://localhost:4001/graphql,users=http://..."`
    pub fn from_env() -> Self {
        Self::new(parse_env(
            &std::env::var("OPEN_RUNO_SUBGRAPHS").unwrap_or_default(),
        ))
        .with_trusted(
            std::env::var("OPEN_RUNO_TRUSTED_SUBGRAPHS")
                .unwrap_or_default()
                .split(',')
                .map(|n| n.trim().to_string()),
        )
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
        let Ok(bytes) = resp.bytes().await else {
            return;
        };
        let Ok(v) = open_runo_router::secure_json::from_slice::<serde_json::Value>(&bytes) else {
            return;
        };
        let mut fields = HashSet::new();
        // Root type names are not always `Query` / `Mutation` (any `schema { query: X }` works).
        let root_name = |root: &str, default: &str| -> String {
            v.pointer(&format!("/data/__schema/{root}/name"))
                .and_then(|n| n.as_str())
                .unwrap_or(default)
                .to_string()
        };
        let (qname, mname) = (
            root_name("queryType", "Query"),
            root_name("mutationType", "Mutation"),
        );
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
        // Optional: the subgraph's SDL, for `@key` entity joins (a subgraph without it still routes).
        if let Ok(r) = self
            .client
            .post(&sg.url)
            .json(&serde_json::json!({ "query": "{ _service { sdl } }" }))
            .send()
            .await
        {
            if let Ok(bytes) = r.bytes().await {
                if let Ok(v) =
                    open_runo_router::secure_json::from_slice::<serde_json::Value>(&bytes)
                {
                    if let Some(sdl) = v.pointer("/data/_service/sdl").and_then(|x| x.as_str()) {
                        let mut m = parse_subgraph_model(sdl);
                        normalize_root_names(&mut m, &qname, &mname);
                        sg.model = Some(m);
                    }
                }
            }
        }
    }

    /// Route one request. `variables` / `operation_name` are forwarded as given.
    pub async fn route(
        &self,
        query: &str,
        operation_name: Option<&str>,
        variables: serde_json::Value,
    ) -> Routed {
        self.route_with_headers(query, operation_name, variables, &[])
            .await
    }

    /// Like [`route`](Self::route), also sending `headers` (callers pass only [`FORWARD_HEADERS`]).
    pub async fn route_with_headers(
        &self,
        query: &str,
        operation_name: Option<&str>,
        variables: serde_json::Value,
        headers: &[(String, String)],
    ) -> Routed {
        let Ok(mut doc) = parse_query(query) else {
            return Routed::Local;
        };
        inline_named_fragments(&mut doc);
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
                        model: None,
                        last_attempt: None,
                    },
                );
                self.discover(&mut sg).await;
                guard[i] = sg;
            }
        }

        let mut targets: Vec<(String, String)> = Vec::new();
        let mut local = false;
        for f in &roots {
            match guard.iter().find(|s| s.fields.contains(f)) {
                Some(s) => {
                    if !targets.iter().any(|(n, _)| *n == s.name) {
                        targets.push((s.name.clone(), s.url.clone()));
                    }
                },
                None => local = true,
            }
        }
        drop(guard);

        if targets.is_empty() {
            return Routed::Local;
        }
        if local {
            return Routed::Rejected(format!(
                "query mixes fields of subgraph `{}` with local fields; cross-subgraph queries are not supported by this gateway yet",
                targets[0].0
            ));
        }
        if targets.len() > 1 {
            return self
                .route_split(&doc, operation_name, &variables, headers, &targets)
                .await;
        }
        if let Some(r) = self
            .try_entity_join(&doc, operation_name, &variables, headers, &targets[0])
            .await
        {
            return r;
        }
        let (name, url) = targets.remove(0);
        self.forward(&name, &url, query, operation_name, &variables, headers)
            .await
    }

    /// `@key` entity join: a read-only query whose fields (at any depth) return an entity type `T`
    /// of subgraph A and select fields that only another subgraph B defines for `T` (both declare
    /// `@key` on `T`). A runs the query without B's fields (plus the key and `__typename`); B is
    /// asked for them through `_entities`; results are merged by position, level by level.
    /// `None` = not applicable (the caller forwards the query as usual).
    async fn try_entity_join(
        &self,
        doc: &ExecutableDocument,
        operation_name: Option<&str>,
        variables: &serde_json::Value,
        headers: &[(String, String)],
        target: &(String, String),
    ) -> Option<Routed> {
        let op = select_operation(doc, operation_name)?;
        // A mutation runs once at its subgraph; the entities it returns are then completed by read-only
        // `_entities` calls to the other subgraphs (nothing is executed twice).
        let root_type = match op.ty {
            OperationType::Query => "Query",
            OperationType::Mutation => "Mutation",
            _ => return None,
        };
        let models: Vec<(String, String, SubgraphModel)> = {
            let guard = self.inner.lock().await;
            guard
                .iter()
                .filter_map(|s| s.model.clone().map(|m| (s.name.clone(), s.url.clone(), m)))
                .collect()
        };
        let a = &models.iter().find(|(n, _, _)| *n == target.0)?.2;

        let mut joins: Vec<Join> = Vec::new();
        let mut query_parts: Vec<String> = Vec::new();
        for sel in &op.selection_set.node.items {
            let Selection::Field(f) = &sel.node else {
                return None;
            };
            let f = &f.node;
            if f.name.node.starts_with("__") {
                return None;
            }
            if !field_included(f, variables)? {
                continue;
            }
            let rkey = f
                .alias
                .as_ref()
                .map_or(f.name.node.as_str(), |a| a.node.as_str())
                .to_string();
            let ret = a
                .types
                .get(root_type)
                .and_then(|q| q.get(f.name.node.as_str()));
            match ret {
                Some(ty) if !f.selection_set.node.items.is_empty() => {
                    let provided = a
                        .provides
                        .get(&(root_type.to_string(), f.name.node.to_string()))
                        .cloned()
                        .unwrap_or_default();
                    query_parts.push(plan_field(
                        f,
                        ty,
                        &provided,
                        &[rkey],
                        a,
                        &models,
                        &target.0,
                        variables,
                        &mut joins,
                    )?);
                },
                _ => query_parts.push(print_field(f, 0, variables)?),
            }
        }
        if joins.is_empty() {
            return None;
        }

        let body_sel = query_parts.join(" ");
        let mut head = query_head(op, &body_sel, &[]);
        if root_type == "Mutation" {
            head = head.replacen("query", "mutation", 1);
        }
        let q = format!("{head} {{ {body_sel} }}");
        let Routed::Forwarded(mut v) = self
            .forward(&target.0, &target.1, &q, None, variables, headers)
            .await
        else {
            return Some(Routed::Rejected(format!(
                "subgraph `{}` could not run the entity query",
                target.0
            )));
        };
        let mut errors: Vec<serde_json::Value> = Vec::new();
        // Shallow joins first, so a deeper join sees the fields a shallower one added.
        joins.sort_by_key(|j| (j.path.len(), j.rank));
        for join in &joins {
            let Some(data) = v.get_mut("data") else {
                continue;
            };
            let mut objs = collect_at(data, &join.path);
            let mut idx: Vec<usize> = Vec::new();
            let mut reps: Vec<serde_json::Value> = Vec::new();
            for (i, o) in objs.iter().enumerate() {
                let mut r = serde_json::Map::new();
                r.insert(
                    "__typename".into(),
                    serde_json::Value::String(join.ty.clone()),
                );
                let mut ok = true;
                for k in join.key.iter().chain(join.requires.iter()) {
                    match o.get(k.as_str()) {
                        Some(x) if !x.is_null() => {
                            r.insert(k.clone(), x.clone());
                        },
                        _ => ok = false,
                    }
                }
                if ok {
                    idx.push(i);
                    reps.push(serde_json::Value::Object(r));
                }
            }
            if reps.is_empty() {
                continue;
            }
            let inner = join.texts.join(" ");
            let ent_sel = format!(
                "_entities(representations: $reps) {{ ... on {} {{ {inner} }} }}",
                join.ty
            );
            let mut vars = variables.as_object().cloned().unwrap_or_default();
            vars.insert("reps".into(), serde_json::Value::Array(reps));
            let head = query_head(op, &inner, &["$reps: [_Any!]!".to_string()]);
            let bq = format!("{head} {{ {ent_sel} }}");
            let (bn, bu, _) = &models[join.b];
            match self
                .forward(bn, bu, &bq, None, &serde_json::Value::Object(vars), headers)
                .await
            {
                Routed::Forwarded(r) => {
                    if let Some(list) = r.pointer("/data/_entities").and_then(|x| x.as_array()) {
                        for (k, ent) in list.iter().enumerate() {
                            if let (Some(&i), Some(m)) = (idx.get(k), ent.as_object()) {
                                if let Some(t) = objs[i].as_object_mut() {
                                    for (fk, fv) in m {
                                        t.insert(fk.clone(), fv.clone());
                                    }
                                }
                            }
                        }
                    }
                    if let Some(e) = r.get("errors").and_then(|e| e.as_array()) {
                        errors.extend(e.iter().cloned());
                    }
                },
                Routed::Rejected(m) => errors.push(serde_json::json!({ "message": m })),
                Routed::Local => {},
            }
        }
        // Remove what was added only to make the joins possible (deepest first is not needed:
        // removals only touch the added key / `__typename` names of each join's own level).
        for join in &joins {
            if join.added.is_empty() {
                continue;
            }
            if let Some(data) = v.get_mut("data") {
                for o in collect_at(data, &join.path) {
                    if let Some(m) = o.as_object_mut() {
                        for k in &join.added {
                            m.remove(k.as_str());
                        }
                    }
                }
            }
        }
        if !errors.is_empty() {
            if let Some(m) = v.as_object_mut() {
                let e = m
                    .entry("errors")
                    .or_insert_with(|| serde_json::Value::Array(Vec::new()));
                if let Some(arr) = e.as_array_mut() {
                    arr.extend(errors);
                }
            }
        }
        Some(Routed::Forwarded(v))
    }

    /// Split a read-only query whose root fields belong to several remote subgraphs into one
    /// sub-query per subgraph, run them concurrently and merge `data` / `errors`.
    /// Mutations are never split (they cannot be made atomic); fragments and directives are
    /// not supported by the splitter and are rejected.
    async fn route_split(
        &self,
        doc: &ExecutableDocument,
        operation_name: Option<&str>,
        variables: &serde_json::Value,
        headers: &[(String, String)],
        targets: &[(String, String)],
    ) -> Routed {
        let names = targets
            .iter()
            .map(|(n, _)| n.as_str())
            .collect::<Vec<_>>()
            .join("` and `");
        let reject =
            |why: &str| Routed::Rejected(format!("query spans subgraphs `{names}`; {why}"));
        let Some(op) = select_operation(doc, operation_name) else {
            return reject("the operation could not be selected");
        };
        if op.ty != OperationType::Query {
            return reject("only read-only queries can be split across subgraphs");
        }
        let owners: Vec<(String, usize)> = {
            let guard = self.inner.lock().await;
            let mut v = Vec::new();
            for sg in guard.iter() {
                if let Some(i) = targets.iter().position(|(n, _)| *n == sg.name) {
                    for f in &sg.fields {
                        v.push((f.clone(), i));
                    }
                }
            }
            v
        };
        let mut parts: Vec<Vec<String>> = vec![Vec::new(); targets.len()];
        for sel in &op.selection_set.node.items {
            let Selection::Field(f) = &sel.node else {
                return reject("fragments at the top level are not supported by the splitter");
            };
            if f.node.name.node.starts_with("__") {
                return reject("introspection cannot be split");
            }
            let Some(text) = print_field(&f.node, 0, variables) else {
                return reject("fragments and directives other than @skip / @include are not supported by the splitter");
            };
            if text.is_empty() {
                continue; // excluded by @skip / @include
            }
            let Some((_, i)) = owners.iter().find(|(n, _)| n == f.node.name.node.as_str()) else {
                return reject("a root field has no owning subgraph");
            };
            parts[*i].push(text);
        }
        let mut calls = Vec::new();
        for (i, (name, url)) in targets.iter().enumerate() {
            let body_sel = parts[i].join(" ");
            let defs = op
                .variable_definitions
                .iter()
                .filter(|d| uses_variable(&body_sel, d.node.name.node.as_str()))
                .map(|d| {
                    let mut t = format!("${}: {}", d.node.name.node, d.node.var_type.node);
                    if let Some(dv) = &d.node.default_value {
                        t.push_str(&format!(" = {}", dv.node));
                    }
                    t
                })
                .collect::<Vec<_>>();
            let head = if defs.is_empty() {
                "query".to_string()
            } else {
                format!("query({})", defs.join(", "))
            };
            calls.push((
                name.clone(),
                url.clone(),
                format!("{head} {{ {body_sel} }}"),
            ));
        }
        let futs = calls
            .iter()
            .map(|(n, u, q)| self.forward(n, u, q, None, variables, headers));
        let results = futures::future::join_all(futs).await;
        let mut data = serde_json::Map::new();
        let mut errors: Vec<serde_json::Value> = Vec::new();
        for r in results {
            match r {
                Routed::Forwarded(v) => {
                    if let Some(d) = v.get("data").and_then(|d| d.as_object()) {
                        for (k, val) in d {
                            data.insert(k.clone(), val.clone());
                        }
                    }
                    if let Some(e) = v.get("errors").and_then(|e| e.as_array()) {
                        errors.extend(e.iter().cloned());
                    }
                },
                Routed::Rejected(m) => errors.push(serde_json::json!({ "message": m })),
                Routed::Local => {},
            }
        }
        let mut out = serde_json::Map::new();
        out.insert("data".into(), serde_json::Value::Object(data));
        if !errors.is_empty() {
            out.insert("errors".into(), serde_json::Value::Array(errors));
        }
        Routed::Forwarded(serde_json::Value::Object(out))
    }

    /// Send one query to one subgraph (headers filtered by trust).
    // 1回の送信に要る値(宛先・問い合わせ・変数・ヘッダー)をそのまま受け取る内部関数。
    #[allow(clippy::too_many_arguments)]
    async fn forward(
        &self,
        name: &str,
        url: &str,
        query: &str,
        operation_name: Option<&str>,
        variables: &serde_json::Value,
        headers: &[(String, String)],
    ) -> Routed {
        let (name, url) = (name.to_string(), url.to_string());
        let body = serde_json::json!({ "query": query, "variables": variables, "operationName": operation_name });
        let creds_ok = self.credentials_allowed(&name, &url);
        let mut req = self.client.post(&url).json(&body);
        for (n, v) in headers {
            // Credential headers only go to a subgraph that is trusted right now.
            if CREDENTIAL_HEADERS.contains(&n.to_ascii_lowercase().as_str()) && !creds_ok {
                continue;
            }
            req = req.header(n.as_str(), v.as_str());
        }
        match req.send().await {
            Ok(r) => match r.bytes().await {
                Ok(bytes) => {
                    match open_runo_router::secure_json::from_slice::<serde_json::Value>(&bytes) {
                        Ok(v) => Routed::Forwarded(v),
                        Err(e) => {
                            // A reply that is not valid (or is oversized / over-nested) JSON: stop trusting it.
                            self.revoke(&name);
                            Routed::Rejected(format!(
                                "subgraph `{name}` returned an invalid response: {e}"
                            ))
                        },
                    }
                },
                Err(e) => {
                    self.revoke(&name);
                    Routed::Rejected(format!(
                        "subgraph `{name}` returned an invalid response: {e}"
                    ))
                },
            },
            Err(e) => Routed::Rejected(format!("subgraph `{name}` is unreachable: {e}")),
        }
    }
}

/// Replace each named fragment spread (`...F`) with an inline fragment (`... on T { .. }`) that has the
/// same type condition, directives and (recursively expanded) selections. The planner then handles it
/// like any inline fragment. A spread of an unknown fragment, a fragment with directives of its own,
/// or a cycle is left as it is (the planner then forwards the query unsplit).
/// Only the parsed copy is changed; the text forwarded to subgraphs is the original.
fn inline_named_fragments(doc: &mut ExecutableDocument) {
    if doc.fragments.is_empty() {
        return;
    }
    let frags = doc.fragments.clone();
    fn walk(
        set: &mut SelectionSet,
        frags: &HashMap<
            async_graphql::Name,
            async_graphql::Positioned<async_graphql::parser::types::FragmentDefinition>,
        >,
        stack: &mut Vec<String>,
    ) {
        for sel in &mut set.items {
            match &mut sel.node {
                Selection::Field(f) => walk(&mut f.node.selection_set.node, frags, stack),
                Selection::InlineFragment(f) => walk(&mut f.node.selection_set.node, frags, stack),
                Selection::FragmentSpread(sp) => {
                    let name = sp.node.fragment_name.node.to_string();
                    let Some(def) = frags.get(&sp.node.fragment_name.node) else {
                        continue;
                    };
                    if !def.node.directives.is_empty() || stack.contains(&name) || stack.len() > 8 {
                        continue;
                    }
                    let mut inner = def.node.selection_set.clone();
                    stack.push(name);
                    walk(&mut inner.node, frags, stack);
                    stack.pop();
                    let frag = InlineFragment {
                        type_condition: Some(def.node.type_condition.clone()),
                        directives: sp.node.directives.clone(),
                        selection_set: inner,
                    };
                    sel.node =
                        Selection::InlineFragment(async_graphql::Positioned::new(frag, sp.pos));
                },
            }
        }
    }
    let mut ops: Vec<&mut async_graphql::Positioned<OperationDefinition>> = Vec::new();
    match &mut doc.operations {
        DocumentOperations::Single(o) => ops.push(o),
        DocumentOperations::Multiple(m) => ops.extend(m.values_mut()),
    }
    for o in ops {
        walk(&mut o.node.selection_set.node, &frags, &mut Vec::new());
    }
}

fn select_operation<'a>(
    doc: &'a ExecutableDocument,
    operation_name: Option<&str>,
) -> Option<&'a OperationDefinition> {
    match &doc.operations {
        DocumentOperations::Single(op) => Some(&op.node),
        DocumentOperations::Multiple(map) => match operation_name {
            Some(n) => map
                .iter()
                .find(|(k, _)| k.as_str() == n)
                .map(|(_, o)| &o.node),
            None if map.len() == 1 => map.values().next().map(|o| &o.node),
            None => None,
        },
    }
}

/// Whether `@skip(if:)` / `@include(if:)` keep the field. The condition is a literal or a variable
/// known now. `None` = a directive the splitter/planner does not understand (or an unknown variable).
fn field_included(f: &Field, vars: &serde_json::Value) -> Option<bool> {
    let mut keep = true;
    for d in &f.directives {
        let name = d.node.name.node.as_str();
        if name != "skip" && name != "include" {
            return None;
        }
        // The argument prints as `true`, `false` or `$name`.
        let arg = d.node.get_argument("if")?.node.to_string();
        let cond = match arg.as_str() {
            "true" => true,
            "false" => false,
            v => vars.get(v.strip_prefix('$')?)?.as_bool()?,
        };
        if (name == "skip" && cond) || (name == "include" && !cond) {
            keep = false;
        }
    }
    Some(keep)
}

/// Print a field (alias, arguments, nested selections). An excluded field (`@skip` / `@include`)
/// prints as an empty string. `None` if it has fragments or directives other than `@skip` / `@include`.
fn print_field(f: &Field, depth: usize, vars: &serde_json::Value) -> Option<String> {
    if depth > 16 {
        return None;
    }
    if !field_included(f, vars)? {
        return Some(String::new());
    }
    let mut o = String::new();
    if let Some(a) = &f.alias {
        o.push_str(a.node.as_str());
        o.push_str(": ");
    }
    o.push_str(f.name.node.as_str());
    if !f.arguments.is_empty() {
        let args = f
            .arguments
            .iter()
            .map(|(k, v)| format!("{}: {}", k.node, v.node))
            .collect::<Vec<_>>();
        o.push_str(&format!("({})", args.join(", ")));
    }
    if !f.selection_set.node.items.is_empty() {
        let mut inner = Vec::new();
        print_selections(&f.selection_set.node.items, depth + 1, vars, &mut inner)?;
        if inner.is_empty() {
            // Everything under this field is excluded: let the plain forwarding path handle it.
            return None;
        }
        o.push_str(&format!(" {{ {} }}", inner.join(" ")));
    }
    Some(o)
}

/// Print a selection list. A fragment without a type condition (`... @include(if: $x) { .. }`) is
/// flattened into its parent (its `@skip` / `@include` are evaluated here). Fragments with a type
/// condition or named spreads give `None` (plain forwarding): flattening them would be wrong for
/// interfaces and unions.
fn print_selections(
    items: &[async_graphql::Positioned<Selection>],
    depth: usize,
    vars: &serde_json::Value,
    out: &mut Vec<String>,
) -> Option<()> {
    if depth > 16 {
        return None;
    }
    for sel in items {
        match &sel.node {
            Selection::Field(c) => {
                let t = print_field(&c.node, depth, vars)?;
                if !t.is_empty() {
                    out.push(t);
                }
            },
            Selection::InlineFragment(f) => {
                if f.node.type_condition.is_some() {
                    return None;
                }
                let mut keep = true;
                for d in &f.node.directives {
                    let name = d.node.name.node.as_str();
                    if name != "skip" && name != "include" {
                        return None;
                    }
                    let arg = d.node.get_argument("if")?.node.to_string();
                    let cond = match arg.as_str() {
                        "true" => true,
                        "false" => false,
                        v => vars.get(v.strip_prefix('$')?)?.as_bool()?,
                    };
                    if (name == "skip" && cond) || (name == "include" && !cond) {
                        keep = false;
                    }
                }
                if keep {
                    print_selections(&f.node.selection_set.node.items, depth + 1, vars, out)?;
                }
            },
            Selection::FragmentSpread(_) => return None,
        }
    }
    Some(())
}

/// The fields of a selection list, with untyped inline fragments flattened (their `@skip` / `@include`
/// evaluated). `None` for typed or named fragments and other directives.
fn flatten_fields<'a>(
    items: &'a [async_graphql::Positioned<Selection>],
    vars: &serde_json::Value,
    depth: usize,
    ty: Option<&str>,
    concrete: bool,
) -> Option<Vec<&'a Field>> {
    if depth > 16 {
        return None;
    }
    let mut out = Vec::new();
    for sel in items {
        match &sel.node {
            Selection::Field(c) => out.push(&c.node),
            Selection::InlineFragment(f) => {
                // `... on T` is the same as the parent only when T is the parent's own (static) type.
                if let Some(tc) = &f.node.type_condition {
                    if Some(tc.node.on.node.as_str()) != ty {
                        // With no interface or union anywhere, a different type can never match:
                        // the fragment contributes nothing (as in GraphQL execution).
                        if concrete && ty.is_some() {
                            continue;
                        }
                        return None;
                    }
                }
                let mut keep = true;
                for d in &f.node.directives {
                    let name = d.node.name.node.as_str();
                    if name != "skip" && name != "include" {
                        return None;
                    }
                    let arg = d.node.get_argument("if")?.node.to_string();
                    let cond = match arg.as_str() {
                        "true" => true,
                        "false" => false,
                        v => vars.get(v.strip_prefix('$')?)?.as_bool()?,
                    };
                    if (name == "skip" && cond) || (name == "include" && !cond) {
                        keep = false;
                    }
                }
                if keep {
                    out.extend(flatten_fields(
                        &f.node.selection_set.node.items,
                        vars,
                        depth + 1,
                        ty,
                        concrete,
                    )?);
                }
            },
            Selection::FragmentSpread(_) => return None,
        }
    }
    Some(out)
}

/// Rename the subgraph's root types to `Query` / `Mutation` inside the model, so the planner can look
/// them up by the standard names whatever the subgraph calls them.
fn normalize_root_names(m: &mut SubgraphModel, query: &str, mutation: &str) {
    for (from, to) in [(query, "Query"), (mutation, "Mutation")] {
        if from == to {
            continue;
        }
        if let Some(t) = m.types.remove(from) {
            m.types.insert(to.to_string(), t);
        }
        if let Some(k) = m.keys.remove(from) {
            m.keys.insert(to.to_string(), k);
        }
        m.externals = std::mem::take(&mut m.externals)
            .into_iter()
            .map(|(t, f)| {
                if t == from {
                    (to.to_string(), f)
                } else {
                    (t, f)
                }
            })
            .collect();
        for map in [&mut m.provides, &mut m.requires] {
            *map = std::mem::take(map)
                .into_iter()
                .map(|((t, f), v)| {
                    (
                        if t == from {
                            (to.to_string(), f)
                        } else {
                            (t, f)
                        },
                        v,
                    )
                })
                .collect();
        }
    }
}

/// One `@key` join point: the objects found at `path` (response keys) of type `ty` get the
/// fields `texts` from subgraph `b` (index into the models list).
struct Join {
    path: Vec<String>,
    b: usize,
    ty: String,
    key: Vec<String>,
    texts: Vec<String>,
    /// Fields of `ty` that subgraph B's `@requires` needs; they are read from subgraph A's answer and
    /// sent in the entity representation together with the key.
    requires: Vec<String>,
    /// Joins run in ascending rank (after the level's own depth): a join whose `@requires` fields come
    /// from another join has a higher rank than that join.
    rank: i32,
    /// Names added only to make the joins possible (removed from the final result).
    added: Vec<String>,
}

/// Make `field` of `ty` available for a representation: `-1` if subgraph A already has it, otherwise the
/// rank of the extra join (against the subgraph that owns it, and recursively its own `@requires`)
/// that supplies it. `None` when no subgraph can supply it.
// 再帰する計画処理の内部関数。文脈を構造体にまとめると呼び出し箇所が大きく変わるため、引数のまま保つ。
#[allow(clippy::too_many_arguments)]
fn supply(
    ty: &str,
    field: &str,
    a: &SubgraphModel,
    models: &[(String, String, SubgraphModel)],
    a_name: &str,
    deps: &mut Vec<(usize, String, Vec<String>)>,
    avail: &mut std::collections::BTreeMap<String, i32>,
    depth: u32,
) -> Option<i32> {
    if depth > 4 {
        return None;
    }
    let key = (ty.to_string(), field.to_string());
    if a.types.get(ty).is_some_and(|t| t.contains_key(field)) && !a.externals.contains(&key) {
        return Some(-1);
    }
    if let Some(r) = avail.get(field) {
        return Some(*r);
    }
    let c = models.iter().position(|(n, _, m)| {
        n != a_name
            && m.keys.contains_key(ty)
            && m.types.get(ty).is_some_and(|t| t.contains_key(field))
            && !m.externals.contains(&key)
    })?;
    let creqs = models[c].2.requires.get(&key).cloned().unwrap_or_default();
    let mut rank = 0;
    for r in &creqs {
        rank = rank.max(supply(ty, r, a, models, a_name, deps, avail, depth + 1)? + 1);
    }
    avail.insert(field.to_string(), rank);
    deps.push((c, field.to_string(), creqs));
    Some(rank)
}

/// Print field `f` (returning entity/object type `ty` of subgraph A) for subgraph A, moving the fields
/// only another subgraph defines into `joins`, recursively through nested object fields.
// 再帰する計画処理の内部関数。文脈を構造体にまとめると呼び出し箇所が大きく変わるため、引数のまま保つ。
#[allow(clippy::too_many_arguments)]
fn plan_field(
    f: &Field,
    ty: &str,
    provided: &[String],
    path: &[String],
    a: &SubgraphModel,
    models: &[(String, String, SubgraphModel)],
    a_name: &str,
    vars: &serde_json::Value,
    joins: &mut Vec<Join>,
) -> Option<String> {
    if path.len() > 8 {
        return None;
    }
    let mut local: Vec<String> = Vec::new();
    let mut ext: Vec<(usize, Vec<String>, Vec<String>)> = Vec::new();
    let mut deps: Vec<(usize, String, Vec<String>)> = Vec::new();
    let mut avail: std::collections::BTreeMap<String, i32> = std::collections::BTreeMap::new();
    let mut selected: Vec<String> = Vec::new();
    for c in flatten_fields(
        &f.selection_set.node.items,
        vars,
        0,
        Some(ty),
        !a.has_abstract,
    )? {
        if !field_included(c, vars)? {
            continue;
        }
        let cname = c.name.node.as_str();
        let ckey = c
            .alias
            .as_ref()
            .map_or(cname, |x| x.node.as_str())
            .to_string();
        if c.alias.is_none() {
            selected.push(cname.to_string());
        }
        // A field marked `@external` is not resolved here, unless the parent field `@provides` it.
        let external = a.externals.contains(&(ty.to_string(), cname.to_string()))
            && !provided.iter().any(|p| p == cname);
        let owned_here = cname == "__typename"
            || (!external && a.types.get(ty).is_some_and(|t| t.contains_key(cname)));
        if !owned_here {
            let other = models.iter().position(|(n, _, m)| {
                n != a_name
                    && m.keys.contains_key(ty)
                    && m.types.get(ty).is_some_and(|t| t.contains_key(cname))
            });
            if let Some(b) = other {
                let text = print_field(c, 1, vars)?;
                // `@requires` of the other subgraph: subgraph A must be able to supply those fields.
                let reqs = models[b]
                    .2
                    .requires
                    .get(&(ty.to_string(), cname.to_string()))
                    .cloned()
                    .unwrap_or_default();
                for r in &reqs {
                    // A supplies it, or another subgraph does (a chain of joins); otherwise no join.
                    supply(ty, r, a, models, a_name, &mut deps, &mut avail, 0)?;
                }
                match ext.iter_mut().find(|(i, _, _)| *i == b) {
                    Some((_, v, rq)) => {
                        v.push(text);
                        for r in reqs {
                            if !rq.contains(&r) {
                                rq.push(r);
                            }
                        }
                    },
                    None => ext.push((b, vec![text], reqs)),
                }
                continue;
            }
        }
        // A nested object field that A owns: look inside it for more joins.
        let child_ty = a.types.get(ty).and_then(|t| t.get(cname));
        match child_ty {
            Some(cty)
                if !c.selection_set.node.items.is_empty() && a.types.contains_key(cty.as_str()) =>
            {
                let mut p = path.to_vec();
                p.push(ckey);
                let child_provided = a
                    .provides
                    .get(&(ty.to_string(), cname.to_string()))
                    .cloned()
                    .unwrap_or_default();
                local.push(plan_field(
                    c,
                    cty,
                    &child_provided,
                    &p,
                    a,
                    models,
                    a_name,
                    vars,
                    joins,
                )?);
            },
            _ => local.push(print_field(c, 1, vars)?),
        }
    }
    let a_owns = |k: &str| {
        a.types.get(ty).is_some_and(|t| t.contains_key(k))
            && !a.externals.contains(&(ty.to_string(), k.to_string()))
    };
    // Joins: the ones the client's fields need, plus the extra ones that supply `@requires` fields.
    let mut specs: Vec<(usize, Vec<String>, Vec<String>)> = ext.clone();
    for (c, field, creqs) in &deps {
        // The client may already have selected that field itself (then it is already one of `ext`).
        if !ext
            .iter()
            .any(|(b, texts, _)| b == c && texts.iter().any(|t| t == field))
        {
            specs.push((*c, vec![field.clone()], creqs.clone()));
        }
    }
    let mut added: Vec<String> = Vec::new(); // added to subgraph A's selection
    let mut strip: Vec<String> = Vec::new(); // supplied by other joins, only removed at the end
    for (_, field, _) in &deps {
        if !selected.contains(field) && !strip.contains(field) {
            strip.push(field.clone());
        }
    }
    for (b, texts, reqs) in &specs {
        let key = models[*b].2.keys[ty].clone();
        for k in key.iter().chain(reqs.iter()) {
            if selected.contains(k) || added.contains(k) || strip.contains(k) {
                continue;
            }
            if a_owns(k) {
                added.push(k.clone());
            }
        }
        let rank = reqs
            .iter()
            .map(|r| avail.get(r).copied().unwrap_or(-1))
            .max()
            .map_or(0, |m| m + 1);
        joins.push(Join {
            path: path.to_vec(),
            b: *b,
            ty: ty.to_string(),
            key,
            texts: texts.clone(),
            requires: reqs.clone(),
            rank,
            added: Vec::new(),
        });
    }
    if !specs.is_empty() && !selected.iter().any(|s| s == "__typename") {
        added.push("__typename".to_string());
    }
    // The names added at this level are removed once, by the first join of this level.
    if let Some(first) = joins.iter_mut().rev().find(|j| j.path == path) {
        first.added = added.iter().chain(strip.iter()).cloned().collect();
    }
    local.extend(added);
    let mut head = String::new();
    if let Some(al) = &f.alias {
        head.push_str(al.node.as_str());
        head.push_str(": ");
    }
    head.push_str(f.name.node.as_str());
    if !f.arguments.is_empty() {
        let args = f
            .arguments
            .iter()
            .map(|(k, v)| format!("{}: {}", k.node, v.node))
            .collect::<Vec<_>>();
        head.push_str(&format!("({})", args.join(", ")));
    }
    Some(format!("{head} {{ {} }}", local.join(" ")))
}

/// Every JSON object reached by following `path` (response keys) from `data`, through arrays.
fn collect_at<'a>(
    data: &'a mut serde_json::Value,
    path: &[String],
) -> Vec<&'a mut serde_json::Value> {
    let mut cur: Vec<&'a mut serde_json::Value> = vec![data];
    for key in path {
        let mut next: Vec<&'a mut serde_json::Value> = Vec::new();
        for node in cur {
            if let Some(child) = node.get_mut(key.as_str()) {
                match child {
                    serde_json::Value::Array(arr) => next.extend(arr.iter_mut()),
                    other => next.push(other),
                }
            }
        }
        cur = next;
    }
    cur.into_iter().filter(|v| v.is_object()).collect()
}

/// `query` or `query($a: T, ...)` with only the operation's variables used in `body` (+ `extra` definitions).
fn query_head(op: &OperationDefinition, body: &str, extra: &[String]) -> String {
    let mut defs: Vec<String> = extra.to_vec();
    for d in &op.variable_definitions {
        if uses_variable(body, d.node.name.node.as_str()) {
            let mut t = format!("${}: {}", d.node.name.node, d.node.var_type.node);
            if let Some(dv) = &d.node.default_value {
                t.push_str(&format!(" = {}", dv.node));
            }
            defs.push(t);
        }
    }
    if defs.is_empty() {
        "query".to_string()
    } else {
        format!("query({})", defs.join(", "))
    }
}

/// Is `$name` used in `text` as a whole variable name (not only a prefix of a longer one)?
fn uses_variable(text: &str, name: &str) -> bool {
    let pat = format!("${name}");
    let mut from = 0;
    while let Some(i) = text[from..].find(&pat) {
        let end = from + i + pat.len();
        match text[end..].chars().next() {
            Some(c) if c.is_ascii_alphanumeric() || c == '_' => from = end,
            _ => return true,
        }
    }
    false
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
    async fn forwards_only_the_allowed_headers() {
        use open_runo_router::hyper_compat::{
            json_response, read_json_body, serve, Handler, Router,
        };
        let h: Handler = Arc::new(move |req, _p| {
            Box::pin(async move {
                let reaxt = req
                    .headers()
                    .get("x-reaxt")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string);
                let auth = req
                    .headers()
                    .get("authorization")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string);
                let body: serde_json::Value = read_json_body(req).await.unwrap_or_default();
                let out = if body["query"].as_str().unwrap_or("").contains("__schema") {
                    serde_json::json!({"data":{"__schema":{"queryType":{"fields":[{"name":"items"}]},"mutationType":null}}})
                } else {
                    serde_json::json!({"data":{"x_reaxt":reaxt,"authorization":auth}})
                };
                json_response(hyper::StatusCode::OK, &out)
            })
        });
        let router = Router::new().route(hyper::Method::POST, "/graphql", h);
        let (addr, handle) = serve(router, "127.0.0.1:0".parse().unwrap()).await.unwrap();
        std::mem::forget(handle);
        let r = RemoteSubgraphs::new(vec![("items".into(), format!("http://{addr}/graphql"))]);
        // 呼び出し側が渡したヘッダはそのまま送られる(許可リストの絞り込みは呼び出し側=FORWARD_HEADERS)
        let sent = vec![("x-reaxt".to_string(), "1".to_string())];
        match r
            .route_with_headers("{ items }", None, serde_json::json!({}), &sent)
            .await
        {
            Routed::Forwarded(v) => {
                assert_eq!(v["data"]["x_reaxt"], "1");
                assert!(v["data"]["authorization"].is_null());
            },
            other => panic!("expected Forwarded, got {other:?}"),
        }
        assert_eq!(FORWARD_HEADERS, &["x-reaxt", "authorization"]);
    }

    #[test]
    fn safe_channel_is_https_or_loopback_only() {
        assert!(is_safe_channel("https://sub.example.com/graphql"));
        assert!(is_safe_channel("http://127.0.0.1:8765/_reaxt/graphql"));
        assert!(is_safe_channel("http://localhost/graphql"));
        assert!(is_safe_channel("http://[::1]:80/graphql"));
        assert!(!is_safe_channel("http://sub.example.com/graphql"));
        assert!(!is_safe_channel("http://127.0.0.1.evil.com/graphql"));
        assert!(!is_safe_channel("http://user@evil.com/graphql"));
        assert!(!is_safe_channel("ftp://127.0.0.1/"));
    }

    #[test]
    fn credentials_need_allow_list_and_safe_channel_and_no_revocation() {
        let url = "http://127.0.0.1:1/graphql";
        let none = RemoteSubgraphs::new(vec![("a".into(), url.into())]);
        assert!(
            !none.credentials_allowed("a", url),
            "empty allow-list trusts nobody"
        );
        let r =
            RemoteSubgraphs::new(vec![("a".into(), url.into())]).with_trusted(["a".to_string()]);
        assert!(r.credentials_allowed("a", url));
        assert!(!r.credentials_allowed("b", url), "not on the list");
        assert!(
            !r.credentials_allowed("a", "http://example.com/graphql"),
            "plain http to a remote host"
        );
        r.revoke("a");
        assert!(!r.credentials_allowed("a", url), "revoked at run time");
    }

    async fn header_echo_subgraph() -> String {
        use open_runo_router::hyper_compat::{
            json_response, read_json_body, serve, Handler, Router,
        };
        let h: Handler = Arc::new(move |req, _p| {
            Box::pin(async move {
                let auth = req
                    .headers()
                    .get("authorization")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string);
                let body: serde_json::Value = read_json_body(req).await.unwrap_or_default();
                let out = if body["query"].as_str().unwrap_or("").contains("__schema") {
                    serde_json::json!({"data":{"__schema":{"queryType":{"fields":[{"name":"me"}]},"mutationType":null}}})
                } else {
                    serde_json::json!({"data":{"authorization":auth}})
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
    async fn authorization_reaches_only_a_trusted_subgraph() {
        let url = header_echo_subgraph().await;
        let sent = vec![("authorization".to_string(), "Bearer t".to_string())];
        // 許可リスト無し: 渡らない
        let r = RemoteSubgraphs::new(vec![("me".into(), url.clone())]);
        match r
            .route_with_headers("{ me }", None, serde_json::json!({}), &sent)
            .await
        {
            Routed::Forwarded(v) => assert!(v["data"]["authorization"].is_null(), "{v}"),
            other => panic!("{other:?}"),
        }
        // 許可リストにある+ループバック: 渡る
        let r =
            RemoteSubgraphs::new(vec![("me".into(), url.clone())]).with_trusted(["me".to_string()]);
        match r
            .route_with_headers("{ me }", None, serde_json::json!({}), &sent)
            .await
        {
            Routed::Forwarded(v) => assert_eq!(v["data"]["authorization"], "Bearer t"),
            other => panic!("{other:?}"),
        }
        // 実行中に信頼を取り消す: 渡らなくなる
        r.revoke("me");
        match r
            .route_with_headers("{ me }", None, serde_json::json!({}), &sent)
            .await
        {
            Routed::Forwarded(v) => assert!(v["data"]["authorization"].is_null(), "{v}"),
            other => panic!("{other:?}"),
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

    #[test]
    fn splitter_prints_fields_and_picks_only_used_variables() {
        let doc = parse_query(
            "query Q($id: ID!, $idx: Int = 3, $n: Int) { a: user(id: $id) { id name } stock(limit: $idx) }",
        )
        .unwrap();
        let op = select_operation(&doc, None).unwrap();
        let printed: Vec<String> = op
            .selection_set
            .node
            .items
            .iter()
            .map(|s| match &s.node {
                Selection::Field(f) => print_field(&f.node, 0, &serde_json::json!({})).unwrap(),
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(printed[0], "a: user(id: $id) { id name }");
        assert_eq!(printed[1], "stock(limit: $idx)");
        // `$id` is not used by the second field just because `$idx` starts with it.
        assert!(uses_variable(&printed[0], "id"));
        assert!(!uses_variable(&printed[1], "id"));
        assert!(uses_variable(&printed[1], "idx"));
        assert!(!uses_variable(&printed[0], "n"));
    }

    #[test]
    fn a_different_type_is_dropped_only_when_the_schema_has_no_abstract_types() {
        let doc = parse_query("{ a { id ... on Admin { secret } } }").unwrap();
        let op = select_operation(&doc, None).unwrap();
        let Selection::Field(f) = &op.selection_set.node.items[0].node else {
            unreachable!()
        };
        let items = &f.node.selection_set.node.items;
        let v = serde_json::json!({});
        let flat = flatten_fields(items, &v, 0, Some("User"), true).unwrap();
        assert_eq!(
            flat.iter()
                .map(|f| f.name.node.as_str())
                .collect::<Vec<_>>(),
            ["id"]
        );
        assert!(flatten_fields(items, &v, 0, Some("User"), false).is_none());
    }

    #[test]
    fn flatten_fields_accepts_on_type_only_for_the_static_type() {
        let doc = parse_query("{ a { id ... on User { name } } }").unwrap();
        let op = select_operation(&doc, None).unwrap();
        let Selection::Field(f) = &op.selection_set.node.items[0].node else {
            unreachable!()
        };
        let v = serde_json::json!({});
        let items = &f.node.selection_set.node.items;
        assert_eq!(
            flatten_fields(items, &v, 0, Some("User"), false)
                .unwrap()
                .len(),
            2
        );
        assert!(flatten_fields(items, &v, 0, Some("Admin"), false).is_none());
        assert!(flatten_fields(items, &v, 0, None, false).is_none());
    }

    #[test]
    fn flatten_fields_handles_untyped_fragments() {
        let doc = parse_query("{ a { id ... @include(if: $x) { name } } }").unwrap();
        let op = select_operation(&doc, None).unwrap();
        let Selection::Field(f) = &op.selection_set.node.items[0].node else {
            unreachable!()
        };
        let n = |x: bool| {
            flatten_fields(
                &f.node.selection_set.node.items,
                &serde_json::json!({"x": x}),
                0,
                None,
                false,
            )
            .unwrap()
            .len()
        };
        assert_eq!((n(true), n(false)), (2, 1));
    }

    #[test]
    fn untyped_inline_fragments_are_flattened_with_directives() {
        let doc =
            parse_query("{ a { id ... @include(if: $x) { name } ... @skip(if: true) { gone } } }")
                .unwrap();
        let op = select_operation(&doc, None).unwrap();
        let Selection::Field(f) = &op.selection_set.node.items[0].node else {
            unreachable!()
        };
        assert_eq!(
            print_field(&f.node, 0, &serde_json::json!({"x": true})).unwrap(),
            "a { id name }"
        );
        assert_eq!(
            print_field(&f.node, 0, &serde_json::json!({"x": false})).unwrap(),
            "a { id }"
        );
        let doc = parse_query("{ a { ... on A { id } } }").unwrap();
        let op = select_operation(&doc, None).unwrap();
        let Selection::Field(f) = &op.selection_set.node.items[0].node else {
            unreachable!()
        };
        assert!(print_field(&f.node, 0, &serde_json::json!({})).is_none());
    }

    #[test]
    fn named_fragments_are_inlined_with_type_condition_and_directives() {
        let mut doc = parse_query("{ a { id ...F @include(if: $x) } } fragment F on User { name ...G } fragment G on User { age }").unwrap();
        inline_named_fragments(&mut doc);
        let op = select_operation(&doc, None).unwrap();
        let Selection::Field(f) = &op.selection_set.node.items[0].node else {
            unreachable!()
        };
        let v = serde_json::json!({"x": true});
        let flat =
            flatten_fields(&f.node.selection_set.node.items, &v, 0, Some("User"), false).unwrap();
        assert_eq!(
            flat.iter()
                .map(|f| f.name.node.as_str())
                .collect::<Vec<_>>(),
            ["id", "name", "age"]
        );
        assert_eq!(
            flatten_fields(
                &f.node.selection_set.node.items,
                &serde_json::json!({"x": false}),
                0,
                Some("User"),
                false
            )
            .unwrap()
            .len(),
            1
        );
        // a different static type stays unflattened
        assert!(flatten_fields(
            &f.node.selection_set.node.items,
            &v,
            0,
            Some("Admin"),
            false
        )
        .is_none());
        // a cycle is left alone instead of looping
        let mut doc = parse_query("{ a { ...F } } fragment F on A { ...F }").unwrap();
        inline_named_fragments(&mut doc);
    }

    #[test]
    fn splitter_refuses_fragments_and_directives() {
        let none = serde_json::json!({});
        let doc = parse_query("{ a @deprecated { id } }").unwrap();
        let op = select_operation(&doc, None).unwrap();
        let Selection::Field(f) = &op.selection_set.node.items[0].node else {
            unreachable!()
        };
        assert!(print_field(&f.node, 0, &none).is_none());
        let doc = parse_query("{ a { ...F } } fragment F on A { id }").unwrap();
        let op = select_operation(&doc, None).unwrap();
        let Selection::Field(f) = &op.selection_set.node.items[0].node else {
            unreachable!()
        };
        assert!(print_field(&f.node, 0, &none).is_none());
    }

    #[test]
    fn skip_and_include_are_decided_at_plan_time() {
        let doc = parse_query(
            "query($on: Boolean!) { a { id name @include(if: $on) email @skip(if: true) tag @skip(if: false) } }",
        )
        .unwrap();
        let op = select_operation(&doc, None).unwrap();
        let Selection::Field(f) = &op.selection_set.node.items[0].node else {
            unreachable!()
        };
        let on = serde_json::json!({"on": true});
        let off = serde_json::json!({"on": false});
        assert_eq!(print_field(&f.node, 0, &on).unwrap(), "a { id name tag }");
        assert_eq!(print_field(&f.node, 0, &off).unwrap(), "a { id tag }");
        // an unknown variable is not guessed
        assert!(print_field(&f.node, 0, &serde_json::json!({})).is_none());
    }

    #[tokio::test]
    async fn joins_entities_across_subgraphs_by_key() {
        use open_runo_router::hyper_compat::{
            json_response, read_json_body, serve, Handler, Router,
        };
        // `users` owns User{id,name}; `reviews` adds User.reviews via @key(fields: "id").
        async fn start(users: bool) -> String {
            let h: Handler = Arc::new(move |req, _p| {
                Box::pin(async move {
                    let body: serde_json::Value = read_json_body(req).await.unwrap_or_default();
                    let q = body["query"].as_str().unwrap_or("");
                    let out = if q.contains("__schema") {
                        let f = if users { "user" } else { "reviewsHealth" };
                        serde_json::json!({"data":{"__schema":{"queryType":{"fields":[{"name":f}]},"mutationType":null}}})
                    } else if q.contains("_service") {
                        let sdl = if users {
                            "type Query { user: User } type User @key(fields: \"id\") { id: ID! name: String }"
                        } else {
                            "type Query { reviewsHealth: String } type User @key(fields: \"id\") { id: ID! reviews: String }"
                        };
                        serde_json::json!({"data":{"_service":{"sdl":sdl}}})
                    } else if q.contains("_entities") {
                        let reps = body["variables"]["reps"]
                            .as_array()
                            .cloned()
                            .unwrap_or_default();
                        let list: Vec<serde_json::Value> = reps
                            .iter()
                            .map(|r| serde_json::json!({"reviews": format!("r-{}", r["id"].as_str().unwrap_or("?"))}))
                            .collect();
                        serde_json::json!({"data":{"_entities":list}})
                    } else {
                        serde_json::json!({"data":{"user":{"name":"Ann","id":"7","__typename":"User"}}})
                    };
                    json_response(hyper::StatusCode::OK, &out)
                })
            });
            let router = Router::new().route(hyper::Method::POST, "/graphql", h);
            let (addr, handle) = serve(router, "127.0.0.1:0".parse().unwrap()).await.unwrap();
            std::mem::forget(handle);
            format!("http://{addr}/graphql")
        }
        let r = RemoteSubgraphs::new(vec![
            ("users".into(), start(true).await),
            ("reviews".into(), start(false).await),
        ]);
        match r
            .route("{ user { name reviews } }", None, serde_json::json!({}))
            .await
        {
            Routed::Forwarded(v) => {
                assert_eq!(
                    v["data"]["user"],
                    serde_json::json!({"name":"Ann","reviews":"r-7"}),
                    "{v}"
                );
                assert!(v.get("errors").is_none(), "{v}");
            },
            _ => panic!("expected a joined result"),
        }
        // Without a field of the other subgraph, the query is forwarded unchanged.
        assert!(matches!(
            r.route("{ user { name } }", None, serde_json::json!({}))
                .await,
            Routed::Forwarded(_)
        ));
    }

    #[tokio::test]
    async fn joins_nested_entities_across_subgraphs() {
        use open_runo_router::hyper_compat::{
            json_response, read_json_body, serve, Handler, Router,
        };
        // `users` owns Query.me -> User -> manager: User; `reviews` adds User.reviews (key id).
        async fn start(users: bool) -> String {
            let h: Handler = Arc::new(move |req, _p| {
                Box::pin(async move {
                    let body: serde_json::Value = read_json_body(req).await.unwrap_or_default();
                    let q = body["query"].as_str().unwrap_or("");
                    let out = if q.contains("__schema") {
                        let f = if users { "me" } else { "reviewsHealth" };
                        serde_json::json!({"data":{"__schema":{"queryType":{"fields":[{"name":f}]},"mutationType":null}}})
                    } else if q.contains("_service") {
                        let sdl = if users {
                            "type Query { me: User } type User @key(fields: \"id\") { id: ID! name: String manager: User }"
                        } else {
                            "type Query { reviewsHealth: String } type User @key(fields: \"id\") { id: ID! reviews: String }"
                        };
                        serde_json::json!({"data":{"_service":{"sdl":sdl}}})
                    } else if q.contains("_entities") {
                        let reps = body["variables"]["reps"]
                            .as_array()
                            .cloned()
                            .unwrap_or_default();
                        let list: Vec<serde_json::Value> = reps
                            .iter()
                            .map(|r| serde_json::json!({"reviews": format!("r-{}", r["id"].as_str().unwrap_or("?"))}))
                            .collect();
                        serde_json::json!({"data":{"_entities":list}})
                    } else {
                        serde_json::json!({"data":{"me":{"id":"1","name":"Ann","__typename":"User","manager":{"id":"2","name":"Bob","__typename":"User"}}}})
                    };
                    json_response(hyper::StatusCode::OK, &out)
                })
            });
            let router = Router::new().route(hyper::Method::POST, "/graphql", h);
            let (addr, handle) = serve(router, "127.0.0.1:0".parse().unwrap()).await.unwrap();
            std::mem::forget(handle);
            format!("http://{addr}/graphql")
        }
        let r = RemoteSubgraphs::new(vec![
            ("users".into(), start(true).await),
            ("reviews".into(), start(false).await),
        ]);
        match r
            .route(
                "{ me { name reviews manager { name reviews } } }",
                None,
                serde_json::json!({}),
            )
            .await
        {
            Routed::Forwarded(v) => {
                assert_eq!(
                    v["data"]["me"],
                    serde_json::json!({"name":"Ann","reviews":"r-1","manager":{"name":"Bob","reviews":"r-2"}}),
                    "{v}"
                );
                assert!(v.get("errors").is_none(), "{v}");
            },
            _ => panic!("expected a nested joined result"),
        }
    }

    #[tokio::test]
    async fn external_fields_are_joined_unless_provided() {
        use open_runo_router::hyper_compat::{
            json_response, read_json_body, serve, Handler, Router,
        };
        // `reviews` returns Review{author: User}; User.name is @external there and only `users` resolves it,
        // while `email` is provided by Review.author @provides(fields: "email").
        async fn start(reviews: bool) -> (String, Arc<std::sync::atomic::AtomicU32>) {
            let entity_calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
            let ec = entity_calls.clone();
            let h: Handler = Arc::new(move |req, _p| {
                let ec = ec.clone();
                Box::pin(async move {
                    let body: serde_json::Value = read_json_body(req).await.unwrap_or_default();
                    let q = body["query"].as_str().unwrap_or("");
                    let out = if q.contains("__schema") {
                        let f = if reviews { "review" } else { "usersHealth" };
                        serde_json::json!({"data":{"__schema":{"queryType":{"fields":[{"name":f}]},"mutationType":null}}})
                    } else if q.contains("_service") {
                        let sdl = if reviews {
                            "type Query { review: Review } type Review @key(fields: \"id\") { id: ID! author: User @provides(fields: \"email\") } type User @key(fields: \"id\") { id: ID! name: String @external email: String @external }"
                        } else {
                            "type Query { usersHealth: String } type User @key(fields: \"id\") { id: ID! name: String email: String }"
                        };
                        serde_json::json!({"data":{"_service":{"sdl":sdl}}})
                    } else if q.contains("_entities") {
                        ec.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        let reps = body["variables"]["reps"]
                            .as_array()
                            .cloned()
                            .unwrap_or_default();
                        let list: Vec<serde_json::Value> = reps
                            .iter()
                            .map(|_| serde_json::json!({"name":"Ann"}))
                            .collect();
                        serde_json::json!({"data":{"_entities":list}})
                    } else {
                        serde_json::json!({"data":{"review":{"id":"r1","author":{"id":"7","__typename":"User","email":"a@example.com"}}}})
                    };
                    json_response(hyper::StatusCode::OK, &out)
                })
            });
            let router = Router::new().route(hyper::Method::POST, "/graphql", h);
            let (addr, handle) = serve(router, "127.0.0.1:0".parse().unwrap()).await.unwrap();
            std::mem::forget(handle);
            (format!("http://{addr}/graphql"), entity_calls)
        }
        let (reviews_url, _) = start(true).await;
        let (users_url, users_calls) = start(false).await;
        let r = RemoteSubgraphs::new(vec![
            ("reviews".into(), reviews_url),
            ("users".into(), users_url),
        ]);
        // `email` is provided by the reviews subgraph: no join. `name` is external: joined from users.
        match r
            .route(
                "{ review { author { email name } } }",
                None,
                serde_json::json!({}),
            )
            .await
        {
            Routed::Forwarded(v) => {
                assert_eq!(
                    v["data"]["review"]["author"],
                    serde_json::json!({"email":"a@example.com","name":"Ann"}),
                    "{v}"
                );
            },
            _ => panic!("expected a joined result"),
        }
        assert_eq!(users_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        // Only the provided field: no entity call at all.
        match r
            .route(
                "{ review { author { email } } }",
                None,
                serde_json::json!({}),
            )
            .await
        {
            Routed::Forwarded(v) => assert_eq!(
                v["data"]["review"]["author"]["email"], "a@example.com",
                "{v}"
            ),
            _ => panic!("expected a forwarded result"),
        }
        assert_eq!(users_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn requires_sends_the_needed_fields_in_the_representation() {
        use open_runo_router::hyper_compat::{
            json_response, read_json_body, serve, Handler, Router,
        };
        // `catalog` owns Product{upc, price, weight}; `shipping` adds shippingEstimate, which
        // @requires "price weight" (external there). The estimate is computed from the representation.
        async fn start(catalog: bool) -> String {
            let h: Handler = Arc::new(move |req, _p| {
                Box::pin(async move {
                    let body: serde_json::Value = read_json_body(req).await.unwrap_or_default();
                    let q = body["query"].as_str().unwrap_or("");
                    let out = if q.contains("__schema") {
                        let f = if catalog { "product" } else { "shippingHealth" };
                        serde_json::json!({"data":{"__schema":{"queryType":{"fields":[{"name":f}]},"mutationType":null}}})
                    } else if q.contains("_service") {
                        let sdl = if catalog {
                            "type Query { product: Product } type Product @key(fields: \"upc\") { upc: String! price: Int weight: Int }"
                        } else {
                            "type Query { shippingHealth: String } type Product @key(fields: \"upc\") { upc: String! price: Int @external weight: Int @external shippingEstimate: Int @requires(fields: \"price weight\") }"
                        };
                        serde_json::json!({"data":{"_service":{"sdl":sdl}}})
                    } else if q.contains("_entities") {
                        let reps = body["variables"]["reps"]
                            .as_array()
                            .cloned()
                            .unwrap_or_default();
                        let list: Vec<serde_json::Value> = reps
                            .iter()
                            .map(|r| {
                                // Only computable when the representation carries price and weight.
                                match (r["price"].as_i64(), r["weight"].as_i64()) {
                                    (Some(p), Some(w)) => {
                                        serde_json::json!({"shippingEstimate": p * w})
                                    },
                                    _ => serde_json::json!({"shippingEstimate": null}),
                                }
                            })
                            .collect();
                        serde_json::json!({"data":{"_entities":list}})
                    } else {
                        serde_json::json!({"data":{"product":{"upc":"p1","price":5,"weight":3,"__typename":"Product"}}})
                    };
                    json_response(hyper::StatusCode::OK, &out)
                })
            });
            let router = Router::new().route(hyper::Method::POST, "/graphql", h);
            let (addr, handle) = serve(router, "127.0.0.1:0".parse().unwrap()).await.unwrap();
            std::mem::forget(handle);
            format!("http://{addr}/graphql")
        }
        let r = RemoteSubgraphs::new(vec![
            ("catalog".into(), start(true).await),
            ("shipping".into(), start(false).await),
        ]);
        // price and weight are NOT selected by the client: the gateway adds them, then removes them.
        match r
            .route(
                "{ product { upc shippingEstimate } }",
                None,
                serde_json::json!({}),
            )
            .await
        {
            Routed::Forwarded(v) => {
                assert_eq!(
                    v["data"]["product"],
                    serde_json::json!({"upc":"p1","shippingEstimate":15}),
                    "{v}"
                );
                assert!(v.get("errors").is_none(), "{v}");
            },
            _ => panic!("expected a joined result"),
        }
        // When the client selects price itself, it stays in the result.
        match r
            .route(
                "{ product { price shippingEstimate } }",
                None,
                serde_json::json!({}),
            )
            .await
        {
            Routed::Forwarded(v) => {
                assert_eq!(
                    v["data"]["product"],
                    serde_json::json!({"price":5,"shippingEstimate":15}),
                    "{v}"
                );
            },
            _ => panic!("expected a joined result"),
        }
    }

    #[tokio::test]
    async fn mutation_result_is_completed_by_a_read_only_join() {
        use open_runo_router::hyper_compat::{
            json_response, read_json_body, serve, Handler, Router,
        };
        async fn start(users: bool) -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
            let seen: Arc<std::sync::Mutex<Vec<String>>> =
                Arc::new(std::sync::Mutex::new(Vec::new()));
            let s2 = seen.clone();
            let h: Handler = Arc::new(move |req, _p| {
                let s2 = s2.clone();
                Box::pin(async move {
                    let body: serde_json::Value = read_json_body(req).await.unwrap_or_default();
                    let q = body["query"].as_str().unwrap_or("");
                    let out = if q.contains("__schema") {
                        if users {
                            serde_json::json!({"data":{"__schema":{"queryType":{"fields":[{"name":"usersHealth"}]},"mutationType":{"fields":[{"name":"createUser"}]}}}})
                        } else {
                            serde_json::json!({"data":{"__schema":{"queryType":{"fields":[{"name":"reviewsHealth"}]},"mutationType":null}}})
                        }
                    } else if q.contains("_service") {
                        let sdl = if users {
                            "type Query { usersHealth: String } type Mutation { createUser: User } type User @key(fields: \"id\") { id: ID! name: String }"
                        } else {
                            "type Query { reviewsHealth: String } type User @key(fields: \"id\") { id: ID! reviews: String }"
                        };
                        serde_json::json!({"data":{"_service":{"sdl":sdl}}})
                    } else if q.contains("_entities") {
                        s2.lock().unwrap().push("entities".into());
                        let reps = body["variables"]["reps"]
                            .as_array()
                            .cloned()
                            .unwrap_or_default();
                        let list: Vec<serde_json::Value> = reps.iter().map(|r| serde_json::json!({"reviews": format!("r-{}", r["id"].as_str().unwrap_or("?"))})).collect();
                        serde_json::json!({"data":{"_entities":list}})
                    } else {
                        s2.lock()
                            .unwrap()
                            .push(q.split_whitespace().next().unwrap_or("").to_string());
                        serde_json::json!({"data":{"createUser":{"name":"Ann","id":"9","__typename":"User"}}})
                    };
                    json_response(hyper::StatusCode::OK, &out)
                })
            });
            let router = Router::new().route(hyper::Method::POST, "/graphql", h);
            let (addr, handle) = serve(router, "127.0.0.1:0".parse().unwrap()).await.unwrap();
            std::mem::forget(handle);
            (format!("http://{addr}/graphql"), seen)
        }
        let (users_url, users_seen) = start(true).await;
        let (reviews_url, reviews_seen) = start(false).await;
        let r = RemoteSubgraphs::new(vec![
            ("users".into(), users_url),
            ("reviews".into(), reviews_url),
        ]);
        match r
            .route(
                "mutation { createUser { name reviews } }",
                None,
                serde_json::json!({}),
            )
            .await
        {
            Routed::Forwarded(v) => {
                assert_eq!(
                    v["data"]["createUser"],
                    serde_json::json!({"name":"Ann","reviews":"r-9"}),
                    "{v}"
                );
                assert!(v.get("errors").is_none(), "{v}");
            },
            _ => panic!("expected a joined mutation result"),
        }
        // The mutation ran exactly once, as a mutation; the other subgraph only saw a read-only `_entities` call.
        assert_eq!(users_seen.lock().unwrap().as_slice(), ["mutation"]);
        assert_eq!(reviews_seen.lock().unwrap().as_slice(), ["entities"]);
    }

    #[tokio::test]
    async fn skip_and_include_decide_which_fields_are_joined() {
        use open_runo_router::hyper_compat::{
            json_response, read_json_body, serve, Handler, Router,
        };
        async fn start(users: bool, calls: Arc<std::sync::atomic::AtomicU32>) -> String {
            let h: Handler = Arc::new(move |req, _p| {
                let calls = calls.clone();
                Box::pin(async move {
                    let body: serde_json::Value = read_json_body(req).await.unwrap_or_default();
                    let q = body["query"].as_str().unwrap_or("");
                    let out = if q.contains("__schema") {
                        let f = if users { "user" } else { "reviewsHealth" };
                        serde_json::json!({"data":{"__schema":{"queryType":{"fields":[{"name":f}]},"mutationType":null}}})
                    } else if q.contains("_service") {
                        let sdl = if users {
                            "type Query { user: User } type User @key(fields: \"id\") { id: ID! name: String }"
                        } else {
                            "type Query { reviewsHealth: String } type User @key(fields: \"id\") { id: ID! reviews: String }"
                        };
                        serde_json::json!({"data":{"_service":{"sdl":sdl}}})
                    } else if q.contains("_entities") {
                        calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        let reps = body["variables"]["reps"]
                            .as_array()
                            .cloned()
                            .unwrap_or_default();
                        let list: Vec<serde_json::Value> = reps
                            .iter()
                            .map(|_| serde_json::json!({"reviews":"r"}))
                            .collect();
                        serde_json::json!({"data":{"_entities":list}})
                    } else {
                        serde_json::json!({"data":{"user":{"id":"7","name":"Ann","__typename":"User"}}})
                    };
                    json_response(hyper::StatusCode::OK, &out)
                })
            });
            let router = Router::new().route(hyper::Method::POST, "/graphql", h);
            let (addr, handle) = serve(router, "127.0.0.1:0".parse().unwrap()).await.unwrap();
            std::mem::forget(handle);
            format!("http://{addr}/graphql")
        }
        let calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let r = RemoteSubgraphs::new(vec![
            ("users".into(), start(true, calls.clone()).await),
            ("reviews".into(), start(false, calls.clone()).await),
        ]);
        let q = "query($on: Boolean!) { user { name reviews @include(if: $on) } }";
        let run = |on: bool| r.route(q, None, serde_json::json!({"on": on}));
        match run(true).await {
            Routed::Forwarded(v) => assert_eq!(
                v["data"]["user"],
                serde_json::json!({"name":"Ann","reviews":"r"}),
                "{v}"
            ),
            _ => panic!("expected a joined result"),
        }
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        // @include(if: false): the field is dropped and the other subgraph is not called at all.
        match run(false).await {
            Routed::Forwarded(v) => assert!(
                v["data"]["user"].get("reviews").is_none() && v["data"]["user"]["name"] == "Ann",
                "{v}"
            ),
            _ => panic!("expected a forwarded result"),
        }
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn requires_chain_through_a_third_subgraph() {
        use open_runo_router::hyper_compat::{
            json_response, read_json_body, serve, Handler, Router,
        };
        // catalog (A) owns Product{upc}; pricing (C) owns price; shipping (B) adds shippingEstimate which
        // @requires price (external there). price is not selected by the client and not in the catalog.
        async fn start(
            role: u8,
            reps_seen: Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
        ) -> String {
            let h: Handler = Arc::new(move |req, _p| {
                let reps_seen = reps_seen.clone();
                Box::pin(async move {
                    let body: serde_json::Value = read_json_body(req).await.unwrap_or_default();
                    let q = body["query"].as_str().unwrap_or("");
                    let out = if q.contains("__schema") {
                        let f = ["product", "pricingHealth", "shippingHealth"][role as usize];
                        serde_json::json!({"data":{"__schema":{"queryType":{"fields":[{"name":f}]},"mutationType":null}}})
                    } else if q.contains("_service") {
                        let sdl = [
                            "type Query { product: Product } type Product @key(fields: \"upc\") { upc: String! }",
                            "type Query { pricingHealth: String } type Product @key(fields: \"upc\") { upc: String! price: Int }",
                            "type Query { shippingHealth: String } type Product @key(fields: \"upc\") { upc: String! price: Int @external shippingEstimate: Int @requires(fields: \"price\") }",
                        ][role as usize];
                        serde_json::json!({"data":{"_service":{"sdl":sdl}}})
                    } else if q.contains("_entities") {
                        let reps = body["variables"]["reps"]
                            .as_array()
                            .cloned()
                            .unwrap_or_default();
                        reps_seen.lock().unwrap().extend(reps.iter().cloned());
                        let list: Vec<serde_json::Value> = reps
                            .iter()
                            .map(|r| match role {
                                1 => serde_json::json!({"price": 5}),
                                _ => serde_json::json!({"shippingEstimate": r["price"].as_i64().map(|p| p * 3)}),
                            })
                            .collect();
                        serde_json::json!({"data":{"_entities":list}})
                    } else {
                        serde_json::json!({"data":{"product":{"upc":"p1","__typename":"Product"}}})
                    };
                    json_response(hyper::StatusCode::OK, &out)
                })
            });
            let router = Router::new().route(hyper::Method::POST, "/graphql", h);
            let (addr, handle) = serve(router, "127.0.0.1:0".parse().unwrap()).await.unwrap();
            std::mem::forget(handle);
            format!("http://{addr}/graphql")
        }
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let r = RemoteSubgraphs::new(vec![
            ("catalog".into(), start(0, seen.clone()).await),
            ("shipping".into(), start(2, seen.clone()).await),
            ("pricing".into(), start(1, seen.clone()).await),
        ]);
        match r
            .route(
                "{ product { upc shippingEstimate } }",
                None,
                serde_json::json!({}),
            )
            .await
        {
            Routed::Forwarded(v) => {
                assert_eq!(
                    v["data"]["product"],
                    serde_json::json!({"upc":"p1","shippingEstimate":15}),
                    "{v}"
                );
                assert!(v.get("errors").is_none(), "{v}");
            },
            _ => panic!("expected a chained joined result"),
        }
        // The shipping representation carried the price that the pricing subgraph supplied.
        assert!(seen
            .lock()
            .unwrap()
            .iter()
            .any(|r| r["price"] == 5 && r["upc"] == "p1"));
    }

    /// Two independent, real GraphQL servers built with `async-graphql`'s own federation support
    /// (their own `_service { sdl }` and `_entities`), joined by the gateway through `@key`.
    #[tokio::test]
    async fn joins_real_async_graphql_federation_subgraphs() {
        use async_graphql::{EmptyMutation, EmptySubscription, Object, Schema, ID};
        use open_runo_router::hyper_compat::{
            json_response, read_json_body, serve, Handler, Router,
        };

        #[derive(Clone)]
        struct UserA {
            id: ID,
        }
        #[Object]
        impl UserA {
            async fn id(&self) -> &ID {
                &self.id
            }
            async fn name(&self) -> String {
                format!("user-{}", self.id.as_str())
            }
        }
        struct QueryA;
        #[Object]
        impl QueryA {
            async fn me(&self) -> UserA {
                UserA { id: ID("7".into()) }
            }
            #[graphql(entity)]
            async fn find_user_by_id(&self, id: ID) -> UserA {
                UserA { id }
            }
        }

        #[derive(Clone)]
        struct UserB {
            id: ID,
        }
        #[Object(name = "UserA")]
        impl UserB {
            async fn id(&self) -> &ID {
                &self.id
            }
            async fn reviews(&self) -> Vec<String> {
                vec![format!("review-of-{}", self.id.as_str())]
            }
        }
        struct QueryB;
        #[Object]
        impl QueryB {
            async fn reviews_health(&self) -> bool {
                true
            }
            #[graphql(entity)]
            async fn find_user_by_id(&self, id: ID) -> UserB {
                UserB { id }
            }
        }

        async fn serve_schema<Q: async_graphql::ObjectType + 'static>(
            schema: Schema<Q, EmptyMutation, EmptySubscription>,
        ) -> String {
            let h: Handler = Arc::new(move |req, _p| {
                let schema = schema.clone();
                Box::pin(async move {
                    let body: serde_json::Value = read_json_body(req).await.unwrap_or_default();
                    let mut request =
                        async_graphql::Request::new(body["query"].as_str().unwrap_or(""));
                    if let Some(v) = body.get("variables") {
                        request = request.variables(async_graphql::Variables::from_json(v.clone()));
                    }
                    let resp = schema.execute(request).await;
                    let out = serde_json::to_value(&resp).unwrap_or_default();
                    json_response(hyper::StatusCode::OK, &out)
                })
            });
            let router = Router::new().route(hyper::Method::POST, "/graphql", h);
            let (addr, handle) = serve(router, "127.0.0.1:0".parse().unwrap()).await.unwrap();
            std::mem::forget(handle);
            format!("http://{addr}/graphql")
        }
        let a = serve_schema(
            Schema::build(QueryA, EmptyMutation, EmptySubscription)
                .enable_federation()
                .finish(),
        )
        .await;
        let b = serve_schema(
            Schema::build(QueryB, EmptyMutation, EmptySubscription)
                .enable_federation()
                .finish(),
        )
        .await;
        let r = RemoteSubgraphs::new(vec![("users".into(), a), ("reviews".into(), b)]);
        match r
            .route("{ me { name reviews } }", None, serde_json::json!({}))
            .await
        {
            Routed::Forwarded(v) => {
                assert_eq!(
                    v["data"]["me"],
                    serde_json::json!({"name":"user-7","reviews":["review-of-7"]}),
                    "{v}"
                );
                assert!(v.get("errors").is_none(), "{v}");
            },
            other => panic!("expected a joined result, got {other:?}"),
        }
    }

    /// The model reads `@external` / `@requires` / `@provides` from the SDL that a real federation
    /// implementation (`async-graphql`) generates, not only from hand-written SDL.
    #[tokio::test]
    async fn model_reads_directives_from_real_async_graphql_sdl() {
        use async_graphql::{EmptyMutation, EmptySubscription, Object, Schema, ID};

        #[derive(Clone)]
        struct Product {
            upc: ID,
        }
        #[Object]
        impl Product {
            async fn upc(&self) -> &ID {
                &self.upc
            }
            #[graphql(external)]
            async fn price(&self) -> Option<i32> {
                None
            }
            #[graphql(requires = "price")]
            async fn shipping_estimate(&self) -> Option<i32> {
                None
            }
        }
        #[derive(Clone)]
        struct Review {
            id: ID,
        }
        #[Object]
        impl Review {
            async fn id(&self) -> &ID {
                &self.id
            }
            #[graphql(provides = "price")]
            async fn product(&self) -> Product {
                Product {
                    upc: ID("p".into()),
                }
            }
        }
        struct Query;
        #[Object]
        impl Query {
            async fn review(&self) -> Review {
                Review { id: ID("r".into()) }
            }
            #[graphql(entity)]
            async fn find_product_by_upc(&self, upc: ID) -> Product {
                Product { upc }
            }
        }
        let schema = Schema::build(Query, EmptyMutation, EmptySubscription)
            .enable_federation()
            .finish();
        let resp = schema.execute("{ _service { sdl } }").await;
        let v = serde_json::to_value(&resp).unwrap();
        let sdl = v["data"]["_service"]["sdl"].as_str().unwrap().to_string();
        let m = parse_subgraph_model(&sdl);
        assert!(m.keys.contains_key("Product"), "{sdl}");
        assert!(
            m.externals
                .contains(&("Product".to_string(), "price".to_string())),
            "{sdl}"
        );
        assert_eq!(
            m.requires[&("Product".to_string(), "shippingEstimate".to_string())],
            vec!["price"],
            "{sdl}"
        );
        assert_eq!(
            m.provides[&("Review".to_string(), "product".to_string())],
            vec!["price"],
            "{sdl}"
        );
        // `upc` is the key, not external.
        assert!(
            !m.externals
                .contains(&("Product".to_string(), "upc".to_string())),
            "{sdl}"
        );
    }

    /// Four independent, real `async-graphql` federation servers: a `@requires` chain
    /// (catalog -> pricing supplies `price` -> shipping computes `shippingEstimate`) and a `@provides`
    /// field that the gateway must NOT fetch from the owning subgraph (reviews provides `price`).
    #[tokio::test]
    async fn requires_chain_and_provides_with_real_async_graphql_servers() {
        use async_graphql::{EmptyMutation, EmptySubscription, Object, Schema, ID};
        use open_runo_router::hyper_compat::{
            json_response, read_json_body, serve, Handler, Router,
        };

        // catalog: Product{upc}
        struct ProductCatalog {
            upc: ID,
        }
        #[Object(name = "Product")]
        impl ProductCatalog {
            async fn upc(&self) -> &ID {
                &self.upc
            }
        }
        struct QueryCatalog;
        #[Object]
        impl QueryCatalog {
            async fn product(&self) -> ProductCatalog {
                ProductCatalog {
                    upc: ID("p1".into()),
                }
            }
            #[graphql(entity)]
            async fn find_product(&self, upc: ID) -> ProductCatalog {
                ProductCatalog { upc }
            }
        }

        // pricing: Product{upc, price} (price = 5)
        struct ProductPricing {
            upc: ID,
        }
        #[Object(name = "Product")]
        impl ProductPricing {
            async fn upc(&self) -> &ID {
                &self.upc
            }
            async fn price(&self) -> Option<i32> {
                Some(5)
            }
        }
        struct QueryPricing;
        #[Object]
        impl QueryPricing {
            async fn pricing_health(&self) -> bool {
                true
            }
            #[graphql(entity)]
            async fn find_product(&self, upc: ID) -> ProductPricing {
                ProductPricing { upc }
            }
        }

        // shipping: Product{upc, price @external, shippingEstimate @requires(price)}
        struct ProductShipping {
            upc: ID,
            price: Option<i32>,
        }
        #[Object(name = "Product")]
        impl ProductShipping {
            async fn upc(&self) -> &ID {
                &self.upc
            }
            #[graphql(external)]
            async fn price(&self) -> Option<i32> {
                self.price
            }
            #[graphql(requires = "price")]
            async fn shipping_estimate(&self) -> Option<i32> {
                self.price.map(|p| p * 3)
            }
        }
        struct QueryShipping;
        #[Object]
        impl QueryShipping {
            async fn shipping_health(&self) -> bool {
                true
            }
            #[graphql(entity)]
            async fn find_product(
                &self,
                #[graphql(key)] upc: ID,
                price: Option<i32>,
            ) -> ProductShipping {
                ProductShipping { upc, price }
            }
        }

        // reviews: Review{id, product: Product @provides(price)}; here price is 9 (pricing would say 5)
        struct ProductReviews {
            upc: ID,
        }
        #[Object(name = "Product")]
        impl ProductReviews {
            async fn upc(&self) -> &ID {
                &self.upc
            }
            #[graphql(external)]
            async fn price(&self) -> Option<i32> {
                Some(9)
            }
        }
        struct ReviewReviews;
        #[Object(name = "Review")]
        impl ReviewReviews {
            async fn id(&self) -> ID {
                ID("r1".into())
            }
            #[graphql(provides = "price")]
            async fn product(&self) -> ProductReviews {
                ProductReviews {
                    upc: ID("p1".into()),
                }
            }
        }
        struct QueryReviews;
        #[Object]
        impl QueryReviews {
            async fn review(&self) -> ReviewReviews {
                ReviewReviews
            }
            #[graphql(entity)]
            async fn find_product(&self, upc: ID) -> ProductReviews {
                ProductReviews { upc }
            }
        }

        async fn serve_schema<Q: async_graphql::ObjectType + 'static>(
            schema: Schema<Q, EmptyMutation, EmptySubscription>,
        ) -> String {
            let h: Handler = Arc::new(move |req, _p| {
                let schema = schema.clone();
                Box::pin(async move {
                    let body: serde_json::Value = read_json_body(req).await.unwrap_or_default();
                    let mut request =
                        async_graphql::Request::new(body["query"].as_str().unwrap_or(""));
                    if let Some(v) = body.get("variables") {
                        request = request.variables(async_graphql::Variables::from_json(v.clone()));
                    }
                    let resp = schema.execute(request).await;
                    json_response(
                        hyper::StatusCode::OK,
                        &serde_json::to_value(&resp).unwrap_or_default(),
                    )
                })
            });
            let router = Router::new().route(hyper::Method::POST, "/graphql", h);
            let (addr, handle) = serve(router, "127.0.0.1:0".parse().unwrap()).await.unwrap();
            std::mem::forget(handle);
            format!("http://{addr}/graphql")
        }
        macro_rules! build {
            ($q:expr) => {
                serve_schema(
                    Schema::build($q, EmptyMutation, EmptySubscription)
                        .enable_federation()
                        .finish(),
                )
                .await
            };
        }
        let r = RemoteSubgraphs::new(vec![
            ("catalog".into(), build!(QueryCatalog)),
            ("pricing".into(), build!(QueryPricing)),
            ("shipping".into(), build!(QueryShipping)),
            ("reviews".into(), build!(QueryReviews)),
        ]);

        // @requires chain: price is neither selected by the client nor known to the catalog.
        match r
            .route(
                "{ product { upc shippingEstimate } }",
                None,
                serde_json::json!({}),
            )
            .await
        {
            Routed::Forwarded(v) => {
                assert_eq!(
                    v["data"]["product"],
                    serde_json::json!({"upc":"p1","shippingEstimate":15}),
                    "{v}"
                );
                assert!(v.get("errors").is_none(), "{v}");
            },
            other => panic!("expected the chained result, got {other:?}"),
        }
        // @provides: reviews supplies price (9) itself; the gateway must not ask pricing (it would say 5).
        match r
            .route(
                "{ review { id product { upc price } } }",
                None,
                serde_json::json!({}),
            )
            .await
        {
            Routed::Forwarded(v) => {
                assert_eq!(v["data"]["review"]["product"]["price"], 9, "{v}");
                assert!(v.get("errors").is_none(), "{v}");
            },
            other => panic!("expected the provided result, got {other:?}"),
        }
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
        // Read-only queries spanning two subgraphs are split and merged.
        match r
            .route("{ items { id } users { id } }", None, serde_json::json!({}))
            .await
        {
            Routed::Forwarded(v) => {
                // The fake subgraph echoes the sub-query it received.
                assert!(v["data"].get("echo").is_some(), "{v}");
                assert!(v.get("errors").is_none(), "{v}");
            },
            _ => panic!("expected a merged result"),
        }
        // Mutations are never split.
        assert!(matches!(
            r.route(
                "mutation { createItem { id } createUser { id } }",
                None,
                serde_json::json!({})
            )
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
