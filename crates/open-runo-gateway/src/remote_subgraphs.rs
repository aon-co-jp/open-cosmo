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
//! (`@key` joins) is not implemented.
//!
//! Configuration: `OPEN_RUNO_SUBGRAPHS="name=http://host:port/graphql,..."`.

use async_graphql::parser::{
    parse_query,
    types::{
        DocumentOperations, ExecutableDocument, Field, OperationDefinition, OperationType,
        Selection, SelectionSet,
    },
};
use open_runo_federation::{parse_subgraph_model, SubgraphModel};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

const INTROSPECTION: &str =
    "{ __schema { queryType { fields { name } } mutationType { fields { name } } } }";
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
            }
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
                if let Ok(v) = open_runo_router::secure_json::from_slice::<serde_json::Value>(&bytes) {
                    if let Some(sdl) = v.pointer("/data/_service/sdl").and_then(|x| x.as_str()) {
                        sg.model = Some(parse_subgraph_model(sdl));
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
        self.route_with_headers(query, operation_name, variables, &[]).await
    }

    /// Like [`route`](Self::route), also sending `headers` (callers pass only [`FORWARD_HEADERS`]).
    pub async fn route_with_headers(
        &self,
        query: &str,
        operation_name: Option<&str>,
        variables: serde_json::Value,
        headers: &[(String, String)],
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

    /// `@key` entity join (one level): a read-only query whose root field returns an entity type
    /// `T` of subgraph A, selecting fields that only another subgraph B defines for `T`
    /// (both declare `@key` on `T`). A runs the query without B's fields (plus the key and
    /// `__typename`); B is asked for them through `_entities`; results are merged by position.
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
        if op.ty != OperationType::Query {
            return None;
        }
        let models: Vec<(String, String, SubgraphModel)> = {
            let guard = self.inner.lock().await;
            guard
                .iter()
                .filter_map(|s| s.model.clone().map(|m| (s.name.clone(), s.url.clone(), m)))
                .collect()
        };
        let a = &models.iter().find(|(n, _, _)| *n == target.0)?.2;

        struct Ext {
            b: usize,
            ty: String,
            key: Vec<String>,
            texts: Vec<String>,
        }
        struct Plan {
            key: String,
            query_text: String,
            ext: Vec<Ext>,
            added: Vec<String>,
        }
        let mut plans: Vec<Plan> = Vec::new();
        let mut any_ext = false;
        for sel in &op.selection_set.node.items {
            let Selection::Field(f) = &sel.node else { return None };
            let f = &f.node;
            if f.name.node.starts_with("__") || !f.directives.is_empty() {
                return None;
            }
            let key = f.alias.as_ref().map_or(f.name.node.as_str(), |a| a.node.as_str()).to_string();
            let leaf = f.selection_set.node.items.is_empty();
            let ret = a.types.get("Query").and_then(|q| q.get(f.name.node.as_str()));
            let (Some(ty), false) = (ret, leaf) else {
                plans.push(Plan { key, query_text: print_field(f, 0)?, ext: Vec::new(), added: Vec::new() });
                continue;
            };
            let mut local: Vec<String> = Vec::new();
            let mut ext: Vec<Ext> = Vec::new();
            let mut selected: Vec<String> = Vec::new();
            for c in &f.selection_set.node.items {
                let Selection::Field(c) = &c.node else { return None };
                let c = &c.node;
                let cname = c.name.node.as_str();
                let text = print_field(c, 1)?;
                if c.alias.is_none() {
                    selected.push(cname.to_string());
                }
                let owned_here = cname == "__typename" || a.types.get(ty).is_some_and(|t| t.contains_key(cname));
                let other = if owned_here {
                    None
                } else {
                    models.iter().position(|(n, _, m)| {
                        *n != target.0
                            && m.keys.contains_key(ty.as_str())
                            && m.types.get(ty.as_str()).is_some_and(|t| t.contains_key(cname))
                    })
                };
                match other {
                    Some(b) => {
                        any_ext = true;
                        match ext.iter_mut().find(|e| e.b == b) {
                            Some(e) => e.texts.push(text),
                            None => ext.push(Ext {
                                b,
                                ty: ty.clone(),
                                key: models[b].2.keys[ty.as_str()].clone(),
                                texts: vec![text],
                            }),
                        }
                    },
                    None => local.push(text),
                }
            }
            let mut added = Vec::new();
            for k in ext.iter().flat_map(|e| e.key.iter()) {
                if !selected.contains(k) && !added.contains(k) {
                    local.push(k.clone());
                    added.push(k.clone());
                }
            }
            if !ext.is_empty() && !selected.iter().any(|s| s == "__typename") {
                local.push("__typename".to_string());
                added.push("__typename".to_string());
            }
            let mut head = String::new();
            if let Some(al) = &f.alias {
                head.push_str(al.node.as_str());
                head.push_str(": ");
            }
            head.push_str(f.name.node.as_str());
            if !f.arguments.is_empty() {
                let args = f.arguments.iter().map(|(k, v)| format!("{}: {}", k.node, v.node)).collect::<Vec<_>>();
                head.push_str(&format!("({})", args.join(", ")));
            }
            plans.push(Plan { key, query_text: format!("{head} {{ {} }}", local.join(" ")), ext, added });
        }
        if !any_ext {
            return None;
        }

        let body_sel = plans.iter().map(|p| p.query_text.as_str()).collect::<Vec<_>>().join(" ");
        let head = query_head(op, &body_sel, &[]);
        let q = format!("{head} {{ {body_sel} }}");
        let Routed::Forwarded(mut v) = self.forward(&target.0, &target.1, &q, None, variables, headers).await else {
            return Some(Routed::Rejected(format!("subgraph `{}` could not run the entity query", target.0)));
        };
        let mut errors: Vec<serde_json::Value> = Vec::new();
        for plan in &plans {
            for ext in &plan.ext {
                let Some(slot) = v.get_mut("data").and_then(|d| d.get_mut(plan.key.as_str())) else { continue };
                let mut objs: Vec<&mut serde_json::Value> = match slot {
                    serde_json::Value::Array(a) => a.iter_mut().collect(),
                    o @ serde_json::Value::Object(_) => vec![o],
                    _ => Vec::new(),
                };
                // Representations, remembering which object each belongs to.
                let mut idx: Vec<usize> = Vec::new();
                let mut reps: Vec<serde_json::Value> = Vec::new();
                for (i, o) in objs.iter().enumerate() {
                    let mut r = serde_json::Map::new();
                    r.insert("__typename".into(), serde_json::Value::String(ext.ty.clone()));
                    let mut ok = true;
                    for k in &ext.key {
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
                let inner = ext.texts.join(" ");
                let ent_sel = format!("_entities(representations: $reps) {{ ... on {} {{ {inner} }} }}", ext.ty);
                let mut vars = variables.as_object().cloned().unwrap_or_default();
                vars.insert("reps".into(), serde_json::Value::Array(reps));
                let head = query_head(op, &inner, &["$reps: [_Any!]!".to_string()]);
                let bq = format!("{head} {{ {ent_sel} }}");
                let (bn, bu, _) = &models[ext.b];
                match self.forward(bn, bu, &bq, None, &serde_json::Value::Object(vars), headers).await {
                    Routed::Forwarded(r) => {
                        if let Some(list) = r.pointer("/data/_entities").and_then(|x| x.as_array()) {
                            for (k, ent) in list.iter().enumerate() {
                                if let (Some(&i), Some(m)) = (idx.get(k), ent.as_object()) {
                                    if let Some(target_obj) = objs[i].as_object_mut() {
                                        for (fk, fv) in m {
                                            target_obj.insert(fk.clone(), fv.clone());
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
        }
        // Remove what was added only to make the join possible.
        for plan in &plans {
            if plan.added.is_empty() {
                continue;
            }
            if let Some(slot) = v.get_mut("data").and_then(|d| d.get_mut(plan.key.as_str())) {
                let objs: Vec<&mut serde_json::Value> = match slot {
                    serde_json::Value::Array(a) => a.iter_mut().collect(),
                    o @ serde_json::Value::Object(_) => vec![o],
                    _ => Vec::new(),
                };
                for o in objs {
                    if let Some(m) = o.as_object_mut() {
                        for k in &plan.added {
                            m.remove(k.as_str());
                        }
                    }
                }
            }
        }
        if !errors.is_empty() {
            if let Some(m) = v.as_object_mut() {
                let e = m.entry("errors").or_insert_with(|| serde_json::Value::Array(Vec::new()));
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
        let reject = |why: &str| Routed::Rejected(format!("query spans subgraphs `{names}`; {why}"));
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
            let Some(text) = print_field(&f.node, 0) else {
                return reject("fragments and directives are not supported by the splitter");
            };
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
            calls.push((name.clone(), url.clone(), format!("{head} {{ {body_sel} }}")));
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
                Ok(bytes) => match open_runo_router::secure_json::from_slice::<serde_json::Value>(&bytes) {
                    Ok(v) => Routed::Forwarded(v),
                    Err(e) => {
                        // A reply that is not valid (or is oversized / over-nested) JSON: stop trusting it.
                        self.revoke(&name);
                        Routed::Rejected(format!(
                            "subgraph `{name}` returned an invalid response: {e}"
                        ))
                    }
                },
                Err(e) => {
                    self.revoke(&name);
                    Routed::Rejected(format!(
                        "subgraph `{name}` returned an invalid response: {e}"
                    ))
                }
            },
            Err(e) => Routed::Rejected(format!("subgraph `{name}` is unreachable: {e}")),
        }
    }
}

fn select_operation<'a>(
    doc: &'a ExecutableDocument,
    operation_name: Option<&str>,
) -> Option<&'a OperationDefinition> {
    match &doc.operations {
        DocumentOperations::Single(op) => Some(&op.node),
        DocumentOperations::Multiple(map) => match operation_name {
            Some(n) => map.iter().find(|(k, _)| k.as_str() == n).map(|(_, o)| &o.node),
            None if map.len() == 1 => map.values().next().map(|o| &o.node),
            None => None,
        },
    }
}

/// Print a field (alias, arguments, nested selections). `None` if it has fragments or directives.
fn print_field(f: &Field, depth: usize) -> Option<String> {
    if depth > 16 || !f.directives.is_empty() {
        return None;
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
        for sel in &f.selection_set.node.items {
            let Selection::Field(c) = &sel.node else { return None };
            inner.push(print_field(&c.node, depth + 1)?);
        }
        o.push_str(&format!(" {{ {} }}", inner.join(" ")));
    }
    Some(o)
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
        use open_runo_router::hyper_compat::{json_response, read_json_body, serve, Handler, Router};
        let h: Handler = Arc::new(move |req, _p| {
            Box::pin(async move {
                let reaxt = req.headers().get("x-reaxt").and_then(|v| v.to_str().ok()).map(str::to_string);
                let auth = req.headers().get("authorization").and_then(|v| v.to_str().ok()).map(str::to_string);
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
        match r.route_with_headers("{ items }", None, serde_json::json!({}), &sent).await {
            Routed::Forwarded(v) => {
                assert_eq!(v["data"]["x_reaxt"], "1");
                assert!(v["data"]["authorization"].is_null());
            }
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
        assert!(!none.credentials_allowed("a", url), "empty allow-list trusts nobody");
        let r = RemoteSubgraphs::new(vec![("a".into(), url.into())]).with_trusted(["a".to_string()]);
        assert!(r.credentials_allowed("a", url));
        assert!(!r.credentials_allowed("b", url), "not on the list");
        assert!(!r.credentials_allowed("a", "http://example.com/graphql"), "plain http to a remote host");
        r.revoke("a");
        assert!(!r.credentials_allowed("a", url), "revoked at run time");
    }

    async fn header_echo_subgraph() -> String {
        use open_runo_router::hyper_compat::{json_response, read_json_body, serve, Handler, Router};
        let h: Handler = Arc::new(move |req, _p| {
            Box::pin(async move {
                let auth = req.headers().get("authorization").and_then(|v| v.to_str().ok()).map(str::to_string);
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
        match r.route_with_headers("{ me }", None, serde_json::json!({}), &sent).await {
            Routed::Forwarded(v) => assert!(v["data"]["authorization"].is_null(), "{v}"),
            other => panic!("{other:?}"),
        }
        // 許可リストにある+ループバック: 渡る
        let r = RemoteSubgraphs::new(vec![("me".into(), url.clone())]).with_trusted(["me".to_string()]);
        match r.route_with_headers("{ me }", None, serde_json::json!({}), &sent).await {
            Routed::Forwarded(v) => assert_eq!(v["data"]["authorization"], "Bearer t"),
            other => panic!("{other:?}"),
        }
        // 実行中に信頼を取り消す: 渡らなくなる
        r.revoke("me");
        match r.route_with_headers("{ me }", None, serde_json::json!({}), &sent).await {
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
                Selection::Field(f) => print_field(&f.node, 0).unwrap(),
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
    fn splitter_refuses_fragments_and_directives() {
        let doc = parse_query("{ a @include(if: true) { id } }").unwrap();
        let op = select_operation(&doc, None).unwrap();
        let Selection::Field(f) = &op.selection_set.node.items[0].node else { unreachable!() };
        assert!(print_field(&f.node, 0).is_none());
        let doc = parse_query("{ a { ...F } } fragment F on A { id }").unwrap();
        let op = select_operation(&doc, None).unwrap();
        let Selection::Field(f) = &op.selection_set.node.items[0].node else { unreachable!() };
        assert!(print_field(&f.node, 0).is_none());
    }

    #[tokio::test]
    async fn joins_entities_across_subgraphs_by_key() {
        use open_runo_router::hyper_compat::{json_response, read_json_body, serve, Handler, Router};
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
                        let reps = body["variables"]["reps"].as_array().cloned().unwrap_or_default();
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
        let r = RemoteSubgraphs::new(vec![("users".into(), start(true).await), ("reviews".into(), start(false).await)]);
        match r.route("{ user { name reviews } }", None, serde_json::json!({})).await {
            Routed::Forwarded(v) => {
                assert_eq!(v["data"]["user"], serde_json::json!({"name":"Ann","reviews":"r-7"}), "{v}");
                assert!(v.get("errors").is_none(), "{v}");
            },
            _ => panic!("expected a joined result"),
        }
        // Without a field of the other subgraph, the query is forwarded unchanged.
        assert!(matches!(
            r.route("{ user { name } }", None, serde_json::json!({})).await,
            Routed::Forwarded(_)
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
