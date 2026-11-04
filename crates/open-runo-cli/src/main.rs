//! `open-runo-cli` -- a small command-line client over `open-runo-router`'s
//! REST API: schema registry register/get/history, federation status, and
//! an OpenAPI spec dump. This is open-runo's answer to Cosmo's `wgc` CLI
//! (see `docs/cosmo-parity.md` 4a, "Powerful CLI (`wgc`相当)").
//!
//! If no `--api-key` is given, the CLI transparently self-issues a
//! short-lived developer key via `POST /api/keys/self-issue` -- the same
//! "a human never has to manage an API key" flow the WASM frontend uses
//! (see `crates/open-runo-router/src/handlers_hyper.rs::self_issue_key_handler`).
//!
//! Schema and federation responses are decoded through
//! `open_runo_api_types`, the same shared types `open-runo-router` and the
//! WASM frontend use -- so a server-side shape change is a compile error
//! here instead of the silent runtime mismatch that shipped in this CLI's
//! first version (see CLAUDE.md HANDOFF, 2026-07-11).

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use open_runo_api_types::{
    DbDeleteResponse, DbRecordListResponse, DbRecordResponse, DbUpsertRequest, FederationStatusResponse,
    RateLimitedResponse, RegisterSchemaRequest, SchemaHistoryResponse, SchemaVersion,
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "open-runo-cli",
    version,
    about = "open-runo command-line client (wgc-equivalent developer tooling)"
)]
struct Cli {
    /// Base URL of the open-runo-router (or open-runo-gateway) instance to talk to.
    #[arg(long, global = true, default_value = "http://localhost:8080", env = "OPEN_RUNO_CLI_BASE_URL")]
    base_url: String,

    /// API key to send as `X-Api-Key`. If omitted, a short-lived developer
    /// key is self-issued automatically (see module docs).
    #[arg(long, global = true, env = "OPEN_RUNO_CLI_API_KEY")]
    api_key: Option<String>,

    /// Print raw JSON responses instead of a human-readable summary.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Schema Registry operations.
    Schema {
        #[command(subcommand)]
        action: SchemaCommand,
    },
    /// Federation status.
    Federation {
        #[command(subcommand)]
        action: FederationCommand,
    },
    /// Fetch the OpenAPI 3.0 spec (`GET /api/openapi.json`).
    Openapi,
    /// Self-issue a fresh developer API key and print it (no other action).
    Login,
    /// DUAL DATABASE key/value operations.
    Db {
        #[command(subcommand)]
        action: DbCommand,
    },
}

#[derive(Subcommand)]
enum DbCommand {
    /// List every record in a table.
    List {
        #[arg(long)]
        table: String,
    },
    /// Fetch a single record by key.
    Get {
        #[arg(long)]
        table: String,
        #[arg(long)]
        key: String,
    },
    /// Insert or update a record.
    Put {
        #[arg(long)]
        table: String,
        #[arg(long)]
        key: String,
        /// JSON value to store.
        #[arg(long)]
        value: String,
    },
    /// Delete a record by key.
    Delete {
        #[arg(long)]
        table: String,
        #[arg(long)]
        key: String,
    },
}

#[derive(Subcommand)]
enum SchemaCommand {
    /// Register (or update) a schema.
    Register {
        #[arg(long)]
        service: String,
        /// Path to a file containing the GraphQL SDL to register.
        #[arg(long)]
        sdl_file: PathBuf,
        #[arg(long, default_value = "local")]
        stage: String,
    },
    /// Fetch the latest version of a schema for a service.
    Get {
        #[arg(long)]
        service: String,
        #[arg(long, default_value = "local")]
        stage: String,
        #[arg(long)]
        namespace: Option<String>,
    },
    /// List every registered version for a service (composition-check /
    /// search equivalent -- see what's already there before registering).
    History {
        #[arg(long)]
        service: String,
        #[arg(long)]
        namespace: Option<String>,
    },
}

#[derive(Subcommand)]
enum FederationCommand {
    /// Show composed federation status (contributing services, type/field counts).
    Status,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let client = reqwest::Client::new();

    let api_key = match &cli.api_key {
        Some(key) => key.clone(),
        None => self_issue_key(&client, &cli.base_url).await?,
    };

    // Schema/federation calls decode into the shared open_runo_api_types
    // structs (typed, so a server-side shape change is a compile error
    // here); Login/Openapi have no shared type (a bare key string, and an
    // arbitrary OpenAPI document) so they stay as raw JSON.
    let body = match &cli.command {
        Command::Login => {
            // Already self-issued above; just confirm it to the user.
            serde_json::json!({ "api_key": api_key })
        }
        Command::Openapi => get::<Value>(&client, &cli.base_url, &api_key, "/api/openapi.json").await?,
        Command::Federation { action } => match action {
            FederationCommand::Status => {
                let resp: FederationStatusResponse =
                    rpc(&client, &cli.base_url, &api_key, "FederationStatus", &serde_json::json!({})).await?;
                serde_json::to_value(resp)?
            }
        },
        Command::Schema { action } => match action {
            SchemaCommand::Register { service, sdl_file, stage } => {
                let sdl = std::fs::read_to_string(sdl_file)
                    .with_context(|| format!("reading SDL file {}", sdl_file.display()))?;
                let payload = RegisterSchemaRequest {
                    service_name: service.clone(),
                    sdl,
                    stage: stage.clone(),
                    namespace: None,
                };
                let resp: SchemaVersion = rpc(&client, &cli.base_url, &api_key, "RegisterSchema", &payload).await?;
                serde_json::to_value(resp)?
            }
            SchemaCommand::Get { service, stage, namespace } => {
                let mut req = serde_json::json!({ "service": service, "stage": stage });
                if let Some(ns) = namespace {
                    req["namespace"] = serde_json::json!(ns);
                }
                let resp: SchemaVersion = rpc(&client, &cli.base_url, &api_key, "GetSchema", &req).await?;
                serde_json::to_value(resp)?
            }
            SchemaCommand::History { service, namespace } => {
                let mut req = serde_json::json!({ "service": service });
                if let Some(ns) = namespace {
                    req["namespace"] = serde_json::json!(ns);
                }
                let resp: SchemaHistoryResponse = rpc(&client, &cli.base_url, &api_key, "GetSchemaHistory", &req).await?;
                serde_json::to_value(resp)?
            }
        },
        Command::Db { action } => match action {
            DbCommand::List { table } => {
                let resp: DbRecordListResponse =
                    rpc(&client, &cli.base_url, &api_key, "DbList", &serde_json::json!({ "table": table })).await?;
                serde_json::to_value(resp)?
            }
            DbCommand::Get { table, key } => {
                let resp: DbRecordResponse =
                    rpc(&client, &cli.base_url, &api_key, "DbGet", &serde_json::json!({ "table": table, "key": key })).await?;
                serde_json::to_value(resp)?
            }
            DbCommand::Put { table, key, value } => {
                let value: Value = serde_json::from_str(value).context("--value must be valid JSON")?;
                let payload = serde_json::json!({ "table": table, "key": key, "value": value });
                let _ = DbUpsertRequest { value: Value::Null };
                let resp: DbRecordResponse = rpc(&client, &cli.base_url, &api_key, "DbPut", &payload).await?;
                serde_json::to_value(resp)?
            }
            DbCommand::Delete { table, key } => {
                let resp: DbDeleteResponse =
                    rpc(&client, &cli.base_url, &api_key, "DbDelete", &serde_json::json!({ "table": table, "key": key })).await?;
                serde_json::to_value(resp)?
            }
        },
    };

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&body)?);
    } else {
        print_human(&cli.command, &body);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_parses_schema_register_args() {
        let cli = Cli::parse_from([
            "open-runo-cli",
            "schema",
            "register",
            "--service",
            "users",
            "--sdl-file",
            "schema.graphql",
        ]);
        match cli.command {
            Command::Schema { action: SchemaCommand::Register { service, stage, .. } } => {
                assert_eq!(service, "users");
                assert_eq!(stage, "local");
            }
            _ => panic!("expected Schema::Register"),
        }
    }
}

async fn self_issue_key(client: &reqwest::Client, base_url: &str) -> Result<String> {
    let resp = client
        .post(format!("{base_url}/open_cosmo.admin.v1.Admin/IssueSelfKey"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .with_context(|| format!("connecting to {base_url}"))?;
    let body: Value = check_status(resp).await?;
    body.get("api_key")
        .and_then(Value::as_str)
        .map(str::to_string)
        .context("self-issue response had no api_key field")
}

async fn get<T: DeserializeOwned>(client: &reqwest::Client, base_url: &str, api_key: &str, path: &str) -> Result<T> {
    let resp = client
        .get(format!("{base_url}{path}"))
        .header("X-Api-Key", api_key)
        .send()
        .await
        .with_context(|| format!("GET {base_url}{path}"))?;
    decode(resp).await
}

/// Connect の単純呼び出し: `POST /open_cosmo.admin.v1.Admin/<Method>`(JSON 本文)。
async fn rpc<Req: serde::Serialize, T: DeserializeOwned>(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    method: &str,
    payload: &Req,
) -> Result<T> {
    let url = format!("{base_url}/open_cosmo.admin.v1.Admin/{method}");
    let resp = client
        .post(&url)
        .header("X-Api-Key", api_key)
        .json(payload)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    decode(resp).await
}

/// Check a response's status and return its JSON body. Non-2xx errors
/// include the server's `X-Request-Id` (see
/// `open_runo_router::middleware_hyper::with_tracing`) so a user can hand
/// a specific ID to whoever reads the server logs, and `429` responses
/// get a friendly retry-after message instead of a raw JSON dump. Shared
/// by [`decode`] and [`self_issue_key`], which both need this same
/// error-shaping but decode the body differently on success.
async fn check_status(resp: reqwest::Response) -> Result<Value> {
    let status = resp.status();
    let request_id = resp
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let body: Value = resp.json().await.context("decoding response as JSON")?;

    if !status.is_success() {
        let suffix = request_id.map(|id| format!(" (request-id: {id})")).unwrap_or_default();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            if let Ok(rate_limited) = serde_json::from_value::<RateLimitedResponse>(body.clone()) {
                bail!("rate limited, retry in {}s{suffix}", rate_limited.retry_after_secs);
            }
        }
        bail!("request failed ({status}){suffix}: {body}");
    }

    Ok(body)
}

/// Decode a response as JSON via [`check_status`], then deserialize the
/// body into `T` -- a confusing deserialize failure on an error body
/// (which won't match `T`) is caught by `check_status` first.
async fn decode<T: DeserializeOwned>(resp: reqwest::Response) -> Result<T> {
    let body = check_status(resp).await?;
    serde_json::from_value(body).context("response JSON did not match the expected shape")
}

fn print_human(command: &Command, body: &Value) {
    match command {
        Command::Login => {
            println!("api_key: {}", body.get("api_key").and_then(Value::as_str).unwrap_or(""));
        }
        Command::Openapi => {
            println!("{}", serde_json::to_string_pretty(body).unwrap_or_default());
        }
        Command::Federation { .. } => {
            let services = body
                .get("contributing_services")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "))
                .unwrap_or_default();
            println!("contributing services: [{services}]");
            println!("types: {}", body.get("type_count").and_then(Value::as_u64).unwrap_or(0));
            println!("fields: {}", body.get("field_count").and_then(Value::as_u64).unwrap_or(0));
        }
        Command::Schema { action } => match action {
            SchemaCommand::History { .. } => {
                let versions = body
                    .get("versions")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                println!("{} version(s):", versions.len());
                for v in versions {
                    print_schema_version(&v);
                }
            }
            SchemaCommand::Register { .. } | SchemaCommand::Get { .. } => {
                print_schema_version(body);
            }
        },
        Command::Db { action } => match action {
            DbCommand::List { .. } => {
                let table = body.get("table").and_then(Value::as_str).unwrap_or("?");
                let records = body.get("records").and_then(Value::as_array).cloned().unwrap_or_default();
                println!("{} record(s) in {table}:", records.len());
                for r in records {
                    println!(
                        "  {}: {}",
                        r.get("key").and_then(Value::as_str).unwrap_or("?"),
                        r.get("value").cloned().unwrap_or(Value::Null)
                    );
                }
            }
            DbCommand::Get { .. } | DbCommand::Put { .. } => {
                println!(
                    "{}/{}: {}",
                    body.get("table").and_then(Value::as_str).unwrap_or("?"),
                    body.get("key").and_then(Value::as_str).unwrap_or("?"),
                    body.get("value").cloned().unwrap_or(Value::Null)
                );
            }
            DbCommand::Delete { .. } => {
                println!(
                    "deleted {}/{}: {}",
                    body.get("table").and_then(Value::as_str).unwrap_or("?"),
                    body.get("key").and_then(Value::as_str).unwrap_or("?"),
                    body.get("deleted").and_then(Value::as_bool).unwrap_or(false)
                );
            }
        },
    }
}

fn print_schema_version(v: &Value) {
    println!(
        "  {} @{} [{}] id={} created_at={}",
        v.get("service_name").and_then(Value::as_str).unwrap_or("?"),
        v.get("stage").and_then(Value::as_str).unwrap_or("?"),
        v.get("namespace").and_then(Value::as_str).unwrap_or("?"),
        v.get("id").and_then(Value::as_str).unwrap_or("?"),
        v.get("created_at").and_then(Value::as_str).unwrap_or("?"),
    );
}
