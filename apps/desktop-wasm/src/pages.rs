//! Page rendering + sidebar navigation. Poem/Tauri/Node-free equivalent of
//! the old `src/main.ts` router and `src/pages/*.ts` — content is set via
//! `innerHTML` (like the old templates) and forms are wired up with
//! `wasm_bindgen::Closure` event listeners instead of TypeScript handlers.

use crate::api;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{Element, HtmlInputElement, HtmlSelectElement, HtmlTextAreaElement};

fn document() -> web_sys::Document {
    web_sys::window().expect("no window").document().expect("no document")
}

fn content_el() -> Option<Element> {
    document().get_element_by_id("content")
}

fn by_id(id: &str) -> Option<Element> {
    document().get_element_by_id(id)
}

fn input_value(id: &str) -> String {
    by_id(id)
        .and_then(|e| e.dyn_into::<HtmlInputElement>().ok())
        .map(|e| e.value())
        .unwrap_or_default()
}

fn textarea_value(id: &str) -> String {
    by_id(id)
        .and_then(|e| e.dyn_into::<HtmlTextAreaElement>().ok())
        .map(|e| e.value())
        .unwrap_or_default()
}

fn select_value(id: &str) -> String {
    by_id(id)
        .and_then(|e| e.dyn_into::<HtmlSelectElement>().ok())
        .map(|e| e.value())
        .unwrap_or_default()
}

/// The first selected `File` from an `<input type="file">`, or `None` if
/// the element is missing or no file has been chosen.
fn file_input_first_file(id: &str) -> Option<web_sys::File> {
    by_id(id)
        .and_then(|e| e.dyn_into::<HtmlInputElement>().ok())
        .and_then(|e| e.files())
        .and_then(|files| files.get(0))
}

fn set_text(id: &str, text: &str) {
    if let Some(el) = by_id(id) {
        el.set_text_content(Some(text));
    }
}

/// Native browser confirm dialog. Used to guard destructive actions
/// (delete, purge-all) so a stray click can't silently discard data.
fn confirm(message: &str) -> bool {
    web_sys::window()
        .and_then(|w| w.confirm_with_message(message).ok())
        .unwrap_or(false)
}

/// Disable/enable a button (or any element) — used to prevent double
/// submission while an async request is in flight.
fn set_disabled(id: &str, disabled: bool) {
    let Some(el) = by_id(id) else { return };
    if disabled {
        let _ = el.set_attribute("disabled", "true");
    } else {
        let _ = el.remove_attribute("disabled");
    }
}

/// Attach `on_click` to element `id`, leaking the closure (fine for a
/// page's lifetime — the whole page is torn down and its listeners
/// dropped together whenever `render_*` overwrites `#content`).
fn on_click(id: &str, on_click: impl Fn() + 'static) {
    let Some(el) = by_id(id) else { return };
    let closure = Closure::<dyn Fn()>::new(on_click);
    let _ = el.add_event_listener_with_callback(
        "click",
        closure.as_ref().unchecked_ref(),
    );
    closure.forget();
}

/// Mount the app: wire up sidebar navigation and render the initial page.
/// Equivalent to the old `main.ts`'s `navigate()` dispatcher.
pub fn mount() {
    let Some(content) = content_el() else {
        web_sys::console::log_1(&JsValue::from_str(
            "host HTML is missing a #content element; aborting mount",
        ));
        return;
    };
    let _ = content;

    for page in ["dashboard", "schemas", "federation", "ai-routing", "db", "scim", "persisted-queries", "feature-flags", "cache-backup", "analytics"] {
        let link_id = format!("nav-{page}");
        on_click(&link_id, move || navigate(page));
    }

    navigate("dashboard");
}

fn navigate(page: &str) {
    let document = document();
    if let Some(nav_list) = document.get_element_by_id("sidebar-nav") {
        let links = nav_list.query_selector_all("a").ok();
        if let Some(links) = links {
            for i in 0..links.length() {
                if let Some(node) = links.item(i) {
                    if let Ok(el) = node.dyn_into::<Element>() {
                        let is_active = el.id() == format!("nav-{page}");
                        let _ = el.set_attribute("data-active", if is_active { "true" } else { "false" });
                    }
                }
            }
        }
    }

    match page {
        "dashboard" => render_dashboard(),
        "schemas" => render_schemas(),
        "federation" => render_federation(),
        "ai-routing" => render_ai_routing(),
        "db" => render_db(),
        "scim" => render_scim(),
        "persisted-queries" => render_persisted_queries(),
        "feature-flags" => render_feature_flags(),
        "cache-backup" => render_cache_backup(),
        "analytics" => render_analytics(),
        _ => {}
    }
}

fn render_dashboard() {
    let Some(content) = content_el() else { return };
    content.set_inner_html(
        r#"<h2>Dashboard</h2><p id="health-status">Checking backend health…</p>"#,
    );
    wasm_bindgen_futures::spawn_local(async move {
        match api::health_check().await {
            Ok(h) => set_text(
                "health-status",
                &format!("{} — {} v{}", h.status, h.service, h.version),
            ),
            Err(e) => set_text("health-status", &format!("health check failed: {e}")),
        }
    });
}

fn render_schemas() {
    let Some(content) = content_el() else { return };
    content.set_inner_html(
        r#"
        <h2>Schema Registry</h2>
        <fieldset>
          <legend>Register Schema</legend>
          <label>Service name <input id="svc-name" placeholder="users-service" /></label><br/>
          <label>Stage
            <select id="svc-stage">
              <option value="local">local</option>
              <option value="development">development</option>
              <option value="staging">staging</option>
              <option value="production">production</option>
            </select>
          </label><br/>
          <label>SDL<br/><textarea id="svc-sdl" rows="4" placeholder="type User { id: ID! }"></textarea></label><br/>
          <button id="register-btn">Register</button>
          <span id="register-msg"></span>
        </fieldset>
        <fieldset>
          <legend>Register Schema from File (multipart upload)</legend>
          <label>Service name <input id="svc-upload-name" placeholder="users-service" /></label><br/>
          <label>Stage
            <select id="svc-upload-stage">
              <option value="local">local</option>
              <option value="development">development</option>
              <option value="staging">staging</option>
              <option value="production">production</option>
            </select>
          </label><br/>
          <label>SDL file <input id="svc-sdl-file" type="file" accept=".graphql,.gql,.txt,text/plain" /></label><br/>
          <button id="upload-btn">Upload</button>
          <span id="upload-msg"></span>
        </fieldset>
        <fieldset>
          <legend>Schema History</legend>
          <input id="hist-svc" placeholder="service name" />
          <button id="hist-btn">Fetch</button>
          <pre id="history-list"></pre>
        </fieldset>
        "#,
    );

    on_click("register-btn", || {
        set_disabled("register-btn", true);
        wasm_bindgen_futures::spawn_local(async move {
            let name = input_value("svc-name");
            let sdl = textarea_value("svc-sdl");
            let stage = select_value("svc-stage");
            set_text("register-msg", "registering…");
            match api::register_schema(&name, &sdl, &stage).await {
                Ok(r) => set_text(
                    "register-msg",
                    &format!(
                        "registered {} @{} in \"{}\" ({}) at {}",
                        r.service_name, r.stage, r.namespace, r.id, r.created_at
                    ),
                ),
                Err(e) => set_text("register-msg", &format!("failed: {e}")),
            }
            set_disabled("register-btn", false);
        });
    });

    on_click("upload-btn", || {
        set_disabled("upload-btn", true);
        wasm_bindgen_futures::spawn_local(async move {
            let name = input_value("svc-upload-name");
            let stage = select_value("svc-upload-stage");
            let Some(file) = file_input_first_file("svc-sdl-file") else {
                set_text("upload-msg", "choose a file first");
                set_disabled("upload-btn", false);
                return;
            };
            set_text("upload-msg", "uploading…");
            match api::register_schema_upload(&name, &stage, &file).await {
                Ok(r) => set_text(
                    "upload-msg",
                    &format!(
                        "registered {} @{} in \"{}\" ({}) at {}",
                        r.service_name, r.stage, r.namespace, r.id, r.created_at
                    ),
                ),
                Err(e) => set_text("upload-msg", &format!("failed: {e}")),
            }
            set_disabled("upload-btn", false);
        });
    });

    on_click("hist-btn", || {
        wasm_bindgen_futures::spawn_local(async move {
            let service = input_value("hist-svc");
            set_text("history-list", "loading…");
            match api::get_schema_history(&service).await {
                Ok(h) => {
                    let lines: Vec<String> = h
                        .versions
                        .iter()
                        .map(|v| {
                            format!(
                                "{} @{} in \"{}\" ({}) at {}",
                                v.service_name, v.stage, v.namespace, v.id, v.created_at
                            )
                        })
                        .collect();
                    set_text("history-list", &lines.join("\n"));
                }
                Err(e) => set_text("history-list", &format!("failed: {e}")),
            }
        });
    });
}

fn render_federation() {
    let Some(content) = content_el() else { return };
    content.set_inner_html(
        r#"
        <h2>Federation</h2>
        <button id="refresh-status-btn">Refresh status</button>
        <pre id="federation-status">Loading…</pre>
        "#,
    );

    let load = || {
        wasm_bindgen_futures::spawn_local(async move {
            set_text("federation-status", "loading…");
            match api::federation_status().await {
                Ok(s) => set_text(
                    "federation-status",
                    &format!(
                        "contributing_services: {}\ntype_count: {}\nfield_count: {}",
                        s.contributing_services.join(", "),
                        s.type_count,
                        s.field_count
                    ),
                ),
                Err(e) => set_text("federation-status", &format!("failed: {e}")),
            }
        });
    };
    load();
    on_click("refresh-status-btn", load);
}

fn render_ai_routing() {
    let Some(content) = content_el() else { return };
    content.set_inner_html(
        r#"
        <h2>AI Routing</h2>
        <p>Picks the best provider between a local model and Anthropic Claude, cost-optimized.</p>
        <button id="route-btn">Route request</button>
        <pre id="route-result"></pre>
        "#,
    );

    on_click("route-btn", || {
        wasm_bindgen_futures::spawn_local(async move {
            set_text("route-result", "routing…");
            let candidates = vec![
                api::AiRouteCandidate {
                    provider: "local_llm",
                    estimated_cost_usd_per_1k_tokens: 0.0,
                    estimated_latency_ms: 900,
                    is_local: true,
                    context_length: 8000,
                },
                api::AiRouteCandidate {
                    provider: "anthropic",
                    estimated_cost_usd_per_1k_tokens: 3.0,
                    estimated_latency_ms: 400,
                    is_local: false,
                    context_length: 200_000,
                },
            ];
            match api::ai_route("cost", candidates).await {
                Ok(r) => set_text(
                    "route-result",
                    &format!(
                        "selected: {} (local={}, cost=${:.2}/1k, latency={}ms)",
                        r.selected_provider, r.is_local, r.estimated_cost_usd_per_1k_tokens, r.estimated_latency_ms
                    ),
                ),
                Err(e) => set_text("route-result", &format!("failed: {e}")),
            }
        });
    });
}

fn render_db() {
    let Some(content) = content_el() else { return };
    content.set_inner_html(
        r#"
        <h2>DUAL DATABASE</h2>
        <fieldset>
          <legend>List table</legend>
          <input id="db-list-table" placeholder="table" />
          <button id="db-list-btn">List</button>
          <pre id="db-list-result"></pre>
        </fieldset>
        <fieldset>
          <legend>Get / Put / Delete record</legend>
          <input id="db-table" placeholder="table" /><br/>
          <input id="db-key" placeholder="key" /><br/>
          <textarea id="db-value" rows="3" placeholder='{"hello":"world"}'></textarea><br/>
          <button id="db-get-btn">Get</button>
          <button id="db-put-btn">Put</button>
          <button id="db-delete-btn">Delete</button>
          <pre id="db-record-result"></pre>
        </fieldset>
        "#,
    );

    on_click("db-list-btn", || {
        wasm_bindgen_futures::spawn_local(async move {
            let table = input_value("db-list-table");
            set_text("db-list-result", "loading…");
            match api::db_list(&table).await {
                Ok(r) => {
                    let lines: Vec<String> = r
                        .records
                        .iter()
                        .map(|item| format!("{}: {}", item.key, item.value))
                        .collect();
                    set_text("db-list-result", &format!("count: {}\n{}", r.count, lines.join("\n")));
                }
                Err(e) => set_text("db-list-result", &format!("failed: {e}")),
            }
        });
    });

    on_click("db-get-btn", || {
        wasm_bindgen_futures::spawn_local(async move {
            let table = input_value("db-table");
            let key = input_value("db-key");
            set_text("db-record-result", "loading…");
            match api::db_get(&table, &key).await {
                Ok(r) => set_text("db-record-result", &format!("{}/{}: {}", r.table, r.key, r.value)),
                Err(e) => set_text("db-record-result", &format!("failed: {e}")),
            }
        });
    });

    on_click("db-put-btn", || {
        set_disabled("db-put-btn", true);
        wasm_bindgen_futures::spawn_local(async move {
            let table = input_value("db-table");
            let key = input_value("db-key");
            let value = textarea_value("db-value");
            set_text("db-record-result", "saving…");
            match api::db_put(&table, &key, &value).await {
                Ok(r) => set_text("db-record-result", &format!("saved {}/{}: {}", r.table, r.key, r.value)),
                Err(e) => set_text("db-record-result", &format!("failed: {e}")),
            }
            set_disabled("db-put-btn", false);
        });
    });

    on_click("db-delete-btn", || {
        let table = input_value("db-table");
        let key = input_value("db-key");
        if !confirm(&format!("Delete {table}/{key}? This cannot be undone.")) {
            return;
        }
        set_disabled("db-delete-btn", true);
        wasm_bindgen_futures::spawn_local(async move {
            set_text("db-record-result", "deleting…");
            match api::db_delete(&table, &key).await {
                Ok(()) => set_text("db-record-result", "deleted"),
                Err(e) => set_text("db-record-result", &format!("failed: {e}")),
            }
            set_disabled("db-delete-btn", false);
        });
    });
}

fn render_scim() {
    let Some(content) = content_el() else { return };
    content.set_inner_html(
        r#"
        <h2>SCIM 2.0 Provisioning</h2>
        <fieldset>
          <legend>Users</legend>
          <button id="scim-refresh-btn">Refresh list</button>
          <pre id="scim-user-list">Loading…</pre>
        </fieldset>
        <fieldset>
          <legend>Create User</legend>
          <input id="scim-username" placeholder="userName (e.g. alice@example.com)" /><br/>
          <input id="scim-roles" placeholder="roles (comma-separated, e.g. developer,admin)" /><br/>
          <button id="scim-create-btn">Create</button>
          <span id="scim-create-msg"></span>
        </fieldset>
        <fieldset>
          <legend>Delete User</legend>
          <input id="scim-delete-id" placeholder="user id" />
          <button id="scim-delete-btn">Delete</button>
          <span id="scim-delete-msg"></span>
        </fieldset>
        "#,
    );

    fn refresh_list() {
        wasm_bindgen_futures::spawn_local(async move {
            set_text("scim-user-list", "loading…");
            match api::scim_list_users().await {
                Ok(list) => {
                    let lines: Vec<String> = list
                        .resources
                        .iter()
                        .map(|u| {
                            format!(
                                "{} ({}) active={} roles=[{}]",
                                u.user_name,
                                u.id,
                                u.active,
                                u.roles.join(", ")
                            )
                        })
                        .collect();
                    set_text(
                        "scim-user-list",
                        &format!("total: {}\n{}", list.total_results, lines.join("\n")),
                    );
                }
                Err(e) => set_text("scim-user-list", &format!("failed: {e}")),
            }
        });
    }

    refresh_list();
    on_click("scim-refresh-btn", refresh_list);

    on_click("scim-create-btn", || {
        set_disabled("scim-create-btn", true);
        wasm_bindgen_futures::spawn_local(async move {
            let user_name = input_value("scim-username");
            let roles_raw = input_value("scim-roles");
            let roles: Vec<&str> = roles_raw
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect();
            set_text("scim-create-msg", "creating…");
            match api::scim_create_user(&user_name, roles).await {
                Ok(_) => {
                    set_text("scim-create-msg", "created");
                    refresh_list();
                }
                Err(e) => set_text("scim-create-msg", &format!("failed: {e}")),
            }
            set_disabled("scim-create-btn", false);
        });
    });

    on_click("scim-delete-btn", || {
        let id = input_value("scim-delete-id");
        if !confirm(&format!("Delete user {id}? This will also revoke their API keys.")) {
            return;
        }
        set_disabled("scim-delete-btn", true);
        wasm_bindgen_futures::spawn_local(async move {
            set_text("scim-delete-msg", "deleting…");
            match api::scim_delete_user(&id).await {
                Ok(()) => {
                    set_text("scim-delete-msg", "deleted");
                    refresh_list();
                }
                Err(e) => set_text("scim-delete-msg", &format!("failed: {e}")),
            }
            set_disabled("scim-delete-btn", false);
        });
    });
}

fn render_persisted_queries() {
    let Some(content) = content_el() else { return };
    content.set_inner_html(
        r#"
        <h2>Persisted Queries / Trusted Documents</h2>
        <fieldset>
          <legend>Register document</legend>
          <textarea id="pq-query" rows="3" placeholder="{ health }"></textarea><br/>
          <button id="pq-register-btn">Register</button>
          <pre id="pq-register-result"></pre>
        </fieldset>
        <fieldset>
          <legend>Fetch by hash</legend>
          <input id="pq-hash" placeholder="sha256 hash" />
          <button id="pq-fetch-btn">Fetch</button>
          <pre id="pq-fetch-result"></pre>
        </fieldset>
        "#,
    );

    on_click("pq-register-btn", || {
        wasm_bindgen_futures::spawn_local(async move {
            let query = textarea_value("pq-query");
            set_text("pq-register-result", "registering…");
            match api::register_persisted_query(&query).await {
                Ok(r) => set_text(
                    "pq-register-result",
                    &format!("hash: {}\nregistered_at: {}", r.hash, r.registered_at),
                ),
                Err(e) => set_text("pq-register-result", &format!("failed: {e}")),
            }
        });
    });

    on_click("pq-fetch-btn", || {
        wasm_bindgen_futures::spawn_local(async move {
            let hash = input_value("pq-hash");
            set_text("pq-fetch-result", "loading…");
            match api::get_persisted_query(&hash).await {
                Ok(r) => set_text(
                    "pq-fetch-result",
                    &format!("hash: {}\nquery: {}\nregistered_at: {}", r.hash, r.query, r.registered_at),
                ),
                Err(e) => set_text("pq-fetch-result", &format!("failed: {e}")),
            }
        });
    });
}

fn render_feature_flags() {
    let Some(content) = content_el() else { return };
    content.set_inner_html(
        r#"
        <h2>Feature Flags</h2>
        <p>Cosmo Feature Flags parity: canary rollouts with deterministic bucketing (same caller always lands on the same side of a rollout_percent split).</p>
        <fieldset>
          <legend>Flags</legend>
          <button id="ff-refresh-btn">Refresh list</button>
          <pre id="ff-list">Loading…</pre>
        </fieldset>
        <fieldset>
          <legend>Create / Update flag</legend>
          <input id="ff-name" placeholder="flag name (e.g. new-checkout)" /><br/>
          <label><input type="checkbox" id="ff-enabled" checked /> enabled</label><br/>
          <label>Rollout % <input id="ff-rollout" type="number" min="0" max="100" value="100" /></label><br/>
          <input id="ff-description" placeholder="description (optional)" /><br/>
          <button id="ff-upsert-btn">Save</button>
          <span id="ff-upsert-msg"></span>
        </fieldset>
        <fieldset>
          <legend>Evaluate</legend>
          <input id="ff-eval-name" placeholder="flag name" />
          <input id="ff-eval-bucket-key" placeholder="bucket key (e.g. user id)" />
          <button id="ff-eval-btn">Evaluate</button>
          <pre id="ff-eval-result"></pre>
        </fieldset>
        <fieldset>
          <legend>Delete flag</legend>
          <input id="ff-delete-name" placeholder="flag name" />
          <button id="ff-delete-btn">Delete</button>
          <span id="ff-delete-msg"></span>
        </fieldset>
        "#,
    );

    fn refresh_list() {
        wasm_bindgen_futures::spawn_local(async move {
            set_text("ff-list", "loading…");
            match api::feature_flag_list().await {
                Ok(list) => {
                    let lines: Vec<String> = list
                        .flags
                        .iter()
                        .map(|f| {
                            format!(
                                "{} enabled={} rollout={}% \"{}\"",
                                f.name, f.enabled, f.rollout_percent, f.description
                            )
                        })
                        .collect();
                    set_text(
                        "ff-list",
                        if lines.is_empty() { "(no flags yet)".to_string() } else { lines.join("\n") }.as_str(),
                    );
                }
                Err(e) => set_text("ff-list", &format!("failed: {e}")),
            }
        });
    }

    refresh_list();
    on_click("ff-refresh-btn", refresh_list);

    on_click("ff-upsert-btn", || {
        set_disabled("ff-upsert-btn", true);
        wasm_bindgen_futures::spawn_local(async move {
            let name = input_value("ff-name");
            let enabled = by_id("ff-enabled")
                .and_then(|e| e.dyn_into::<web_sys::HtmlInputElement>().ok())
                .map(|e| e.checked())
                .unwrap_or(true);
            let rollout_percent: u8 = input_value("ff-rollout").parse().unwrap_or(100);
            let description = input_value("ff-description");
            set_text("ff-upsert-msg", "saving…");
            match api::feature_flag_upsert(&name, enabled, rollout_percent, &description).await {
                Ok(f) => {
                    set_text(
                        "ff-upsert-msg",
                        &format!("saved: {} enabled={} rollout={}%", f.name, f.enabled, f.rollout_percent),
                    );
                    refresh_list();
                }
                Err(e) => set_text("ff-upsert-msg", &format!("failed: {e}")),
            }
            set_disabled("ff-upsert-btn", false);
        });
    });

    on_click("ff-eval-btn", || {
        wasm_bindgen_futures::spawn_local(async move {
            let name = input_value("ff-eval-name");
            let bucket_key = input_value("ff-eval-bucket-key");
            set_text("ff-eval-result", "evaluating…");
            match api::feature_flag_evaluate(&name, &bucket_key).await {
                Ok(r) => set_text(
                    "ff-eval-result",
                    &format!("{} @ bucket \"{}\": enabled={}", r.name, r.bucket_key, r.enabled),
                ),
                Err(e) => set_text("ff-eval-result", &format!("failed: {e}")),
            }
        });
    });

    on_click("ff-delete-btn", || {
        let name = input_value("ff-delete-name");
        if !confirm(&format!("Delete feature flag \"{name}\"? This cannot be undone.")) {
            return;
        }
        set_disabled("ff-delete-btn", true);
        wasm_bindgen_futures::spawn_local(async move {
            set_text("ff-delete-msg", "deleting…");
            match api::feature_flag_delete(&name).await {
                Ok(()) => {
                    set_text("ff-delete-msg", "deleted");
                    refresh_list();
                }
                Err(e) => set_text("ff-delete-msg", &format!("failed: {e}")),
            }
            set_disabled("ff-delete-btn", false);
        });
    });
}

fn render_cache_backup() {
    let Some(content) = content_el() else { return };
    content.set_inner_html(
        r#"
        <h2>Cache &amp; Backup</h2>
        <fieldset>
          <legend>HTML page cache</legend>
          <input id="cache-purge-path" placeholder="/page/123" />
          <button id="cache-purge-btn">Purge one</button>
          <button id="cache-purge-all-btn">Purge all</button>
          <button id="cache-stats-btn">AI stats</button>
          <pre id="cache-result"></pre>
        </fieldset>
        <fieldset>
          <legend>Backup &amp; integrity</legend>
          <button id="backup-export-btn">Export backup</button>
          <button id="integrity-check-btn">Run integrity check</button>
          <pre id="backup-result"></pre>
        </fieldset>
        "#,
    );

    on_click("cache-purge-btn", || {
        wasm_bindgen_futures::spawn_local(async move {
            let path = input_value("cache-purge-path");
            set_text("cache-result", "purging…");
            match api::cache_purge(&path).await {
                Ok(r) => set_text("cache-result", &format!("purged: {}", r.purged)),
                Err(e) => set_text("cache-result", &format!("failed: {e}")),
            }
        });
    });

    on_click("cache-purge-all-btn", || {
        if !confirm("Purge the entire HTML page cache?") {
            return;
        }
        set_disabled("cache-purge-all-btn", true);
        wasm_bindgen_futures::spawn_local(async move {
            set_text("cache-result", "purging all…");
            match api::cache_purge_all().await {
                Ok(r) => {
                    set_text("cache-result", &format!("purged: {}", r.purged));
                    crate::notifications::notify("Cache purge complete", &format!("purged: {}", r.purged));
                }
                Err(e) => {
                    set_text("cache-result", &format!("failed: {e}"));
                    crate::notifications::notify("Cache purge failed", &e);
                }
            }
            set_disabled("cache-purge-all-btn", false);
        });
    });

    on_click("cache-stats-btn", || {
        wasm_bindgen_futures::spawn_local(async move {
            set_text("cache-result", "loading…");
            match api::cache_ai_stats().await {
                Ok(s) => set_text(
                    "cache-result",
                    &format!(
                        "ai_enabled={} hits={} misses={} hit_ratio={:.2} tracked_keys={}",
                        s.ai_enabled, s.cache_hits, s.cache_misses, s.hit_ratio, s.tracked_keys
                    ),
                ),
                Err(e) => set_text("cache-result", &format!("failed: {e}")),
            }
        });
    });

    on_click("backup-export-btn", || {
        wasm_bindgen_futures::spawn_local(async move {
            set_text("backup-result", "exporting…");
            match api::backup_export().await {
                Ok(r) => {
                    let summary = format!("records={}\nwritten:\n{}", r.records, r.written.join("\n"));
                    set_text("backup-result", &summary);
                    crate::notifications::notify(
                        "Backup export complete",
                        &format!("{} record(s) exported", r.records),
                    );
                }
                Err(e) => {
                    set_text("backup-result", &format!("failed: {e}"));
                    crate::notifications::notify("Backup export failed", &e);
                }
            }
        });
    });

    on_click("integrity-check-btn", || {
        wasm_bindgen_futures::spawn_local(async move {
            set_text("backup-result", "checking…");
            match api::integrity_check().await {
                Ok(r) => {
                    set_text("backup-result", &format!("backend={} healed={}", r.backend, r.healed));
                    crate::notifications::notify(
                        "Integrity check complete",
                        &format!("backend={} healed={}", r.backend, r.healed),
                    );
                }
                Err(e) => {
                    set_text("backup-result", &format!("failed: {e}"));
                    crate::notifications::notify("Integrity check failed", &e);
                }
            }
        });
    });
}

/// Analytics page (Cosmo Studio parity, `docs/cosmo-parity.md` 4a):
/// monthly request-count metering + per-operation latency/error-rate,
/// both served from `open_runo_observability::RequestMetrics`'s
/// in-process aggregates via `GET /api/analytics/requests-per-month` and
/// `GET /api/analytics/operations`. An operational dashboard, not a
/// billing/rate-limit surface -- see those handlers' doc comments.
fn render_analytics() {
    let Some(content) = content_el() else { return };
    content.set_inner_html(
        r#"
        <h2>Analytics</h2>
        <fieldset>
          <legend>Requests per month</legend>
          <button id="analytics-refresh-btn">Refresh</button>
          <table id="analytics-months-table">
            <thead><tr><th>Month</th><th>Requests</th></tr></thead>
            <tbody id="analytics-months-body"></tbody>
          </table>
        </fieldset>
        <fieldset>
          <legend>Per-operation latency &amp; error rate</legend>
          <table id="analytics-ops-table">
            <thead><tr><th>Method</th><th>Path</th><th>Count</th><th>Errors</th><th>Avg ms</th><th>Error rate</th></tr></thead>
            <tbody id="analytics-ops-body"></tbody>
          </table>
        </fieldset>
        <pre id="analytics-error"></pre>
        "#,
    );

    fn load() {
        wasm_bindgen_futures::spawn_local(async move {
            match api::requests_per_month().await {
                Ok(resp) => {
                    if let Some(body) = by_id("analytics-months-body") {
                        let rows: String = resp
                            .months
                            .iter()
                            .map(|m| format!("<tr><td>{}</td><td>{}</td></tr>", m.month, m.count))
                            .collect();
                        body.set_inner_html(&rows);
                    }
                }
                Err(e) => set_text("analytics-error", &format!("requests-per-month failed: {e}")),
            }

            match api::operations_summary().await {
                Ok(resp) => {
                    if let Some(body) = by_id("analytics-ops-body") {
                        let rows: String = resp
                            .operations
                            .iter()
                            .map(|op| {
                                format!(
                                    "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{:.1}</td><td>{:.1}%</td></tr>",
                                    op.method,
                                    op.path,
                                    op.count,
                                    op.error_count,
                                    op.avg_duration_ms,
                                    op.error_rate * 100.0,
                                )
                            })
                            .collect();
                        body.set_inner_html(&rows);
                    }
                }
                Err(e) => set_text("analytics-error", &format!("operations failed: {e}")),
            }
        });
    }

    on_click("analytics-refresh-btn", load);
    load();
}
