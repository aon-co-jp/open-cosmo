//! EDFS (Event-Driven Federated Subscriptions) — Cosmo-parity gap
//! ("Event-Driven Federated Subscriptions (EDFS) / Cosmo Streams",
//! `docs/cosmo-parity.md` §4a). Real Cosmo Streams lets Kafka/NATS/Redis
//! act as the event source behind GraphQL Subscriptions, instead of only
//! an in-process broadcaster. This module closes that gap for the Redis
//! case (the one already used elsewhere in this codebase, via
//! `open-runo-cache`'s `redis-backend` feature) so subscriptions work
//! correctly across a load-balanced, multi-instance deployment: an event
//! published by the instance that handled a mutation is delivered to a
//! subscriber connected to any *other* instance, not just the one that
//! produced it.
//!
//! **Design**: rather than replacing [`crate::state::AppState`]'s existing
//! `broadcast::Sender<SchemaEvent>` (which every GraphQL Subscription
//! consumer already subscribes to via `state.events.subscribe()`), this
//! module *bridges* it: a background task subscribes to a Redis Pub/Sub
//! channel and forwards every message it receives into the local
//! `broadcast::Sender`. Publishing (`publish`) is a separate, explicit
//! step call sites opt into after a local event is produced (see
//! `handlers_hyper::register_schema_and_respond`), publishing the same
//! event to Redis so every *other* instance's bridge task picks it up too.
//! Existing subscription consumer code needs zero changes -- it already
//! reads from `state.events`, which now also carries cross-instance
//! events.
//!
//! Gated behind the `edfs` Cargo feature (`dep:redis`, matching
//! `open-runo-cache`'s `redis-backend` feature's exact `redis` version) so
//! a deployment that only needs in-process Subscriptions (the existing
//! default) doesn't pull in a Redis client it never uses.
//!
//! **Verification note**: this module's Redis-touching functions
//! (`publish`, `spawn_bridge`) require a live Redis/KeyDB/DragonflyDB
//! server and are not covered by an automated test in this sandbox --
//! consistent with `open-runo-cache::redis_backend::RedisCache`, which
//! has the same limitation for the same reason (no Redis server available
//! in this development environment). The message-encoding/decoding logic
//! itself (`encode_event`/`decode_event`) is pure and fully unit-tested
//! without needing a live server.

#[cfg(feature = "edfs")]
use crate::state::{AppState, SchemaEvent};
#[cfg(feature = "edfs")]
use std::sync::Arc;

/// Serialize a [`SchemaEvent`] to the JSON string published on the Redis
/// channel. A thin wrapper around `serde_json`, factored out so the wire
/// format is one obvious place to look at/change, and so it can be unit
/// tested without a live Redis connection.
#[cfg(feature = "edfs")]
pub fn encode_event(event: &SchemaEvent) -> Result<String, serde_json::Error> {
    serde_json::to_string(event)
}

/// Parse a Redis Pub/Sub message payload back into a [`SchemaEvent`].
/// Returns `Err` on malformed payloads (e.g. a message on the channel
/// from something other than this bridge) rather than panicking --
/// a Redis channel is not a type-safe boundary the way an in-process
/// `broadcast::Sender<SchemaEvent>` is.
#[cfg(feature = "edfs")]
pub fn decode_event(payload: &str) -> Result<SchemaEvent, serde_json::Error> {
    serde_json::from_str(payload)
}

/// Publish `event` to `channel` on the Redis server at `redis_url`.
/// Opens a short-lived connection per call rather than holding one open --
/// schema registrations (the only current caller) are infrequent enough
/// that connection setup cost isn't a concern, and it keeps this function
/// simple and independently testable/callable without needing a
/// long-lived handle threaded through `AppState`.
///
/// Failure to publish (Redis down, bad URL, etc.) is deliberately
/// non-fatal to the caller: the schema registration that triggered the
/// event has already succeeded locally and been broadcast in-process by
/// the time this runs (see `handlers_hyper::register_schema_and_respond`),
/// so a Redis hiccup should degrade to "this event doesn't reach other
/// instances" rather than fail the mutation itself.
#[cfg(feature = "edfs")]
pub async fn publish(redis_url: &str, channel: &str, event: &SchemaEvent) -> anyhow::Result<()> {
    let payload = encode_event(event)?;
    let client = redis::Client::open(redis_url)?;
    let mut conn = client.get_multiplexed_async_connection().await?;
    redis::cmd("PUBLISH")
        .arg(channel)
        .arg(payload)
        .query_async::<i64>(&mut conn)
        .await?;
    Ok(())
}

/// Connect to `redis_url`, subscribe to `channel`, and spawn a background
/// task that forwards every message received into `state.events` (the
/// existing in-process broadcaster). Returns once the initial connection
/// and subscription succeed (so a caller can distinguish "Redis
/// unreachable at startup" from "subscribed and running") — the message
/// loop itself runs for the lifetime of the returned `JoinHandle`, in the
/// background.
///
/// A `broadcast::Sender::send` failure here means there are currently no
/// receivers (no active GraphQL Subscription connections on this
/// instance) — not an error condition for a broadcaster; the message is
/// correctly dropped rather than logged as a failure.
#[cfg(feature = "edfs")]
pub async fn spawn_bridge(
    state: Arc<AppState>,
    redis_url: &str,
    channel: &str,
) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    use futures::StreamExt;

    let client = redis::Client::open(redis_url)?;
    let mut pubsub = client.get_async_pubsub().await?;
    pubsub.subscribe(channel).await?;

    let handle = tokio::spawn(async move {
        let mut stream = pubsub.on_message();
        while let Some(msg) = stream.next().await {
            let payload: String = match msg.get_payload() {
                Ok(p) => p,
                Err(error) => {
                    tracing::warn!(%error, "EDFS: received non-UTF8 Redis Pub/Sub payload; dropping");
                    continue;
                }
            };
            match decode_event(&payload) {
                Ok(event) => {
                    // Errors here just mean "nobody is subscribed right
                    // now" -- not logged as a failure (see doc comment).
                    let _ = state.events.send(event);
                }
                Err(error) => {
                    tracing::warn!(%error, payload, "EDFS: received malformed SchemaEvent payload; dropping");
                }
            }
        }
    });

    Ok(handle)
}

#[cfg(all(test, feature = "edfs"))]
mod tests {
    use super::*;

    fn sample_event() -> SchemaEvent {
        SchemaEvent {
            service_name: "orders".to_string(),
            stage: "production".to_string(),
            at: "2026-07-12T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn event_round_trips_through_encode_decode() {
        let event = sample_event();
        let payload = encode_event(&event).expect("encoding a SchemaEvent cannot fail");
        let decoded = decode_event(&payload).expect("decoding a just-encoded payload cannot fail");
        assert_eq!(decoded.service_name, event.service_name);
        assert_eq!(decoded.stage, event.stage);
        assert_eq!(decoded.at, event.at);
    }

    #[test]
    fn decode_event_rejects_malformed_payload() {
        // Not valid JSON at all.
        assert!(decode_event("not json").is_err());
        // Valid JSON, but missing required SchemaEvent fields.
        assert!(decode_event(r#"{"unrelated": true}"#).is_err());
    }

    #[test]
    fn encode_event_produces_valid_json_with_expected_fields() {
        let event = sample_event();
        let payload = encode_event(&event).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(parsed["service_name"], "orders");
        assert_eq!(parsed["stage"], "production");
        assert_eq!(parsed["at"], "2026-07-12T00:00:00Z");
    }
}
