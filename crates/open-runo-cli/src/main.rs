//! `open-runo-cli` -- a small command-line client over open-runo's admin gRPC
//! service (`open_runo.admin.v1.Admin`): schema registry register/get/history,
//! federation status, DB key/value operations, and the service's `.proto`. This
//! is open-runo's answer to Cosmo's `wgc` CLI (see `docs/cosmo-parity.md` 4a).
//!
//! 通信はREST全廃・gRPC移行(CLAUDE.md「2026-11-01追記」)により**gRPC(HTTP/2)**。
//! `http://`は平文(開発・閉域用)、`https://`は**TLS 1.3のみ**。管理系の相互認証(mTLS)は
//! `--client-cert`/`--client-key`、サーバー証明書のCAは`--ca-file`で渡す。
//!
//! If no `--api-key` is given, the CLI transparently self-issues a
//! short-lived developer key via the `SelfIssueKey` RPC -- the same
//! "a human never has to manage an API key" flow the admin UI uses.

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use open_runo_proto::client::{ClientConfig, GrpcClient};
use open_runo_proto::{struct_to_json, v1};
use serde_json::{json, Value};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "open-runo-cli",
    version,
    about = "open-runo command-line client (wgc-equivalent developer tooling, gRPC)"
)]
struct Cli {
    /// gRPCエンドポイント(`http://host:port` 平文h2c / `https://host:port` TLS1.3)。管理画面と同じポート(8080)で受ける。
    #[arg(
        long,
        global = true,
        alias = "base-url",
        default_value = "http://127.0.0.1:8080",
        env = "OPEN_RUNO_CLI_ENDPOINT"
    )]
    endpoint: String,

    /// API key to send as the `x-api-key` metadata. If omitted, a short-lived
    /// developer key is self-issued automatically (see module docs).
    #[arg(long, global = true, env = "OPEN_RUNO_CLI_API_KEY")]
    api_key: Option<String>,

    /// サーバー証明書を検証するCAのPEMファイル(未指定ならWebPKIのルート)。
    #[arg(long, global = true, env = "OPEN_RUNO_CLI_CA_FILE")]
    ca_file: Option<PathBuf>,

    /// mTLS用クライアント証明書チェーン(PEM)。`--client-key`と一緒に指定する。
    #[arg(long, global = true, env = "OPEN_RUNO_CLI_CLIENT_CERT")]
    client_cert: Option<PathBuf>,

    /// mTLS用クライアント秘密鍵(PEM)。
    #[arg(long, global = true, env = "OPEN_RUNO_CLI_CLIENT_KEY")]
    client_key: Option<PathBuf>,

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
    /// Print the admin service definition (`admin.proto`, the source of truth).
    Proto,
    /// Self-issue a fresh developer API key and print it (no other action).
    Login,
    /// DUAL DATABASE key/value operations.
    Db {
        #[command(subcommand)]
        action: DbCommand,
    },
    /// Feature flag operations.
    Flag {
        #[command(subcommand)]
        action: FlagCommand,
    },
    /// Watch admin events (schema registrations, history changes) for a while.
    Events {
        /// 受け取る件数の上限。
        #[arg(long, default_value_t = 1)]
        count: usize,
        /// 待つ秒数(超えたら打ち切る)。
        #[arg(long, default_value_t = 30)]
        timeout_secs: u64,
    },
}

#[derive(Subcommand)]
enum FlagCommand {
    /// List every feature flag.
    List,
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

fn schema_json(v: &v1::SchemaVersion) -> Value {
    json!({
        "id": v.id, "namespace": v.namespace, "service_name": v.service_name,
        "sdl": v.sdl, "stage": v.stage, "created_at": v.created_at,
    })
}

fn read_pem(path: &Option<PathBuf>, what: &str) -> Result<Option<Vec<u8>>> {
    path.as_ref()
        .map(|p| std::fs::read(p).with_context(|| format!("reading {what} {}", p.display())))
        .transpose()
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut cfg = ClientConfig {
        endpoint: cli.endpoint.clone(),
        api_key: cli.api_key.clone(),
        ca_pem: read_pem(&cli.ca_file, "CA file")?,
        client_cert_pem: read_pem(&cli.client_cert, "client cert")?,
        client_key_pem: read_pem(&cli.client_key, "client key")?,
        extra_metadata: Vec::new(),
    };

    // `proto`は接続不要。
    if matches!(cli.command, Command::Proto) {
        println!("{}", open_runo_proto::ADMIN_PROTO);
        return Ok(());
    }

    if cfg.api_key.is_none() {
        let anon = GrpcClient::new(cfg.clone());
        let issued: v1::SelfIssued = anon
            .unary("SelfIssueKey", &v1::Empty {})
            .await
            .map_err(|s| anyhow::anyhow!("{s}"))
            .context("self-issuing an API key")?;
        cfg.api_key = Some(issued.api_key);
    }
    let api_key = cfg.api_key.clone().unwrap_or_default();
    let c = GrpcClient::new(cfg);
    let err = |s: open_runo_proto::client::Status| anyhow::anyhow!("request failed: {s}");

    let body = match &cli.command {
        Command::Proto => unreachable!("handled above"),
        Command::Login => json!({ "api_key": api_key }),
        Command::Federation { action } => match action {
            FederationCommand::Status => {
                let r: v1::FederationStatusResponse = c
                    .unary("FederationStatus", &v1::Empty {})
                    .await
                    .map_err(err)?;
                json!({
                    "contributing_services": r.contributing_services,
                    "type_count": r.type_count,
                    "field_count": r.field_count,
                })
            },
        },
        Command::Schema { action } => match action {
            SchemaCommand::Register {
                service,
                sdl_file,
                stage,
            } => {
                let sdl = std::fs::read_to_string(sdl_file)
                    .with_context(|| format!("reading SDL file {}", sdl_file.display()))?;
                let v: v1::SchemaVersion = c
                    .unary(
                        "RegisterSchema",
                        &v1::RegisterSchemaRequest {
                            service_name: service.clone(),
                            sdl,
                            stage: Some(stage.clone()),
                            namespace: None,
                        },
                    )
                    .await
                    .map_err(err)?;
                schema_json(&v)
            },
            SchemaCommand::Get {
                service,
                stage,
                namespace,
            } => {
                let v: v1::SchemaVersion = c
                    .unary(
                        "GetSchema",
                        &v1::GetSchemaRequest {
                            service_name: service.clone(),
                            stage: Some(stage.clone()),
                            namespace: namespace.clone(),
                        },
                    )
                    .await
                    .map_err(err)?;
                schema_json(&v)
            },
            SchemaCommand::History { service, namespace } => {
                let r: v1::SchemaHistoryResponse = c
                    .unary(
                        "SchemaHistory",
                        &v1::SchemaHistoryRequest {
                            service_name: service.clone(),
                            namespace: namespace.clone(),
                        },
                    )
                    .await
                    .map_err(err)?;
                json!({ "versions": r.versions.iter().map(schema_json).collect::<Vec<_>>() })
            },
        },
        Command::Db { action } => {
            let s: open_runo_proto::prost_types::Struct = match action {
                DbCommand::List { table } => c
                    .unary(
                        "DbList",
                        &v1::DbTable {
                            table: table.clone(),
                        },
                    )
                    .await
                    .map_err(err)?,
                DbCommand::Get { table, key } => c
                    .unary(
                        "DbGet",
                        &v1::DbKey {
                            table: table.clone(),
                            key: key.clone(),
                        },
                    )
                    .await
                    .map_err(err)?,
                DbCommand::Put { table, key, value } => {
                    let value: Value =
                        serde_json::from_str(value).context("--value must be valid JSON")?;
                    c.unary(
                        "DbPut",
                        &v1::DbPutRequest {
                            table: table.clone(),
                            key: key.clone(),
                            value: Some(open_runo_proto::json_to_value(&value)),
                        },
                    )
                    .await
                    .map_err(err)?
                },
                DbCommand::Delete { table, key } => c
                    .unary(
                        "DbDelete",
                        &v1::DbKey {
                            table: table.clone(),
                            key: key.clone(),
                        },
                    )
                    .await
                    .map_err(err)?,
            };
            struct_to_json(&s)
        },
        Command::Flag { action } => match action {
            FlagCommand::List => {
                let r: v1::ListFeatureFlagsResponse = c
                    .unary("ListFeatureFlags", &v1::Empty {})
                    .await
                    .map_err(err)?;
                json!({ "flags": r.flags.iter().map(|f| json!({
                    "name": f.name, "enabled": f.enabled,
                    "rollout_percent": f.rollout_percent, "description": f.description,
                })).collect::<Vec<_>>() })
            },
        },
        Command::Events {
            count,
            timeout_secs,
        } => {
            let evs: Vec<v1::AdminEvent> = c
                .stream_some(
                    "WatchEvents",
                    &v1::Empty {},
                    *count,
                    std::time::Duration::from_secs(*timeout_secs),
                )
                .await
                .map_err(err)?;
            json!({ "events": evs.iter().map(|e| match &e.event {
                Some(v1::admin_event::Event::Schema(s)) => json!({
                    "type": "schema", "service_name": s.service_name, "stage": s.stage, "at": s.at }),
                Some(v1::admin_event::Event::History(h)) => json!({
                    "type": "history", "history_len": h.history_len }),
                None => Value::Null,
            }).collect::<Vec<_>>() })
        },
    };

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&body)?);
    } else {
        print_human(&cli.command, &body);
    }
    if body.is_null() {
        bail!("empty response");
    }
    Ok(())
}

fn print_human(command: &Command, body: &Value) {
    match command {
        Command::Login => {
            println!(
                "api_key: {}",
                body.get("api_key").and_then(Value::as_str).unwrap_or("")
            );
        },
        Command::Proto => {},
        Command::Flag { .. } => {
            let flags = body["flags"].as_array().cloned().unwrap_or_default();
            println!("{} flag(s):", flags.len());
            for f in flags {
                println!(
                    "  {} enabled={} rollout={}% {}",
                    f["name"].as_str().unwrap_or("?"),
                    f["enabled"],
                    f["rollout_percent"],
                    f["description"].as_str().unwrap_or("")
                );
            }
        },
        Command::Events { .. } => {
            let evs = body["events"].as_array().cloned().unwrap_or_default();
            println!("{} event(s):", evs.len());
            for e in evs {
                println!("  {e}");
            }
        },
        Command::Federation { .. } => {
            let services = body
                .get("contributing_services")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            println!("contributing services: [{services}]");
            println!(
                "types: {}",
                body.get("type_count").and_then(Value::as_u64).unwrap_or(0)
            );
            println!(
                "fields: {}",
                body.get("field_count").and_then(Value::as_u64).unwrap_or(0)
            );
        },
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
            },
            SchemaCommand::Register { .. } | SchemaCommand::Get { .. } => {
                print_schema_version(body);
            },
        },
        Command::Db { action } => match action {
            DbCommand::List { .. } => {
                let table = body.get("table").and_then(Value::as_str).unwrap_or("?");
                let records = body
                    .get("records")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                println!("{} record(s) in {table}:", records.len());
                for r in records {
                    println!(
                        "  {}: {}",
                        r.get("key").and_then(Value::as_str).unwrap_or("?"),
                        r.get("value").cloned().unwrap_or(Value::Null)
                    );
                }
            },
            DbCommand::Get { .. } | DbCommand::Put { .. } => {
                println!(
                    "{}/{}: {}",
                    body.get("table").and_then(Value::as_str).unwrap_or("?"),
                    body.get("key").and_then(Value::as_str).unwrap_or("?"),
                    body.get("value").cloned().unwrap_or(Value::Null)
                );
            },
            DbCommand::Delete { .. } => {
                println!(
                    "deleted {}/{}: {}",
                    body.get("table").and_then(Value::as_str).unwrap_or("?"),
                    body.get("key").and_then(Value::as_str).unwrap_or("?"),
                    body.get("deleted")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                );
            },
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
            Command::Schema {
                action: SchemaCommand::Register { service, stage, .. },
            } => {
                assert_eq!(service, "users");
                assert_eq!(stage, "local");
            },
            _ => panic!("expected Schema::Register"),
        }
    }
}
