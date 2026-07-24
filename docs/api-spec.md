# open-cosmo API Specification

All API routes require the `X-Api-Key: <key>` request header.  
Health routes (`/health`, `/healthz`) are public and exempt.

---

## Health

### `GET /health`
### `GET /healthz`

Returns service status. Used by load balancers and Kubernetes liveness probes.

**Response 200**
```json
{
  "status": "ok",
  "service": "open-runo-router",
  "version": "0.1.0"
}
```

---

## Schema Registry

### `POST /api/schemas`

Register a new schema version for a service.

**Request body**
```json
{
  "service_name": "users",
  "sdl": "type User { id: ID! name: String }",
  "stage": "local"
}
```

| Field          | Type   | Required | Default   | Values                              |
|----------------|--------|----------|-----------|-------------------------------------|
| `service_name` | string | ✅       | —         | any                                 |
| `sdl`          | string | ✅       | —         | GraphQL SDL or schema text          |
| `stage`        | string | —        | `"local"` | `local` `development` `staging` `production` |

**Response 200**
```json
{
  "id": "a1b2c3d4-...",
  "service_name": "users",
  "stage": "local",
  "created_at": "2026-07-03T10:00:00Z"
}
```

---

### `GET /api/schemas/:service`

Get the latest registered schema for a service.

**Query parameters**

| Param   | Default  | Description                        |
|---------|----------|------------------------------------|
| `stage` | `local`  | Target stage to query              |

**Response 200**
```json
{
  "id": "a1b2c3d4-...",
  "service_name": "users",
  "sdl": "type User { id: ID! name: String }",
  "stage": "local",
  "created_at": "2026-07-03T10:00:00Z"
}
```

**Response 404** — no schema found for service at given stage.

---

### `GET /api/schemas/:service/history`

Full version history for a service (all stages, oldest first).

**Response 200**
```json
{
  "versions": [
    {
      "id": "...",
      "service_name": "users",
      "sdl": "type User { id: ID! }",
      "stage": "local",
      "created_at": "2026-07-01T09:00:00Z"
    },
    {
      "id": "...",
      "service_name": "users",
      "sdl": "type User { id: ID! name: String }",
      "stage": "local",
      "created_at": "2026-07-03T10:00:00Z"
    }
  ]
}
```

---

## Federation Engine

### `POST /api/federation/compose`

Compose multiple service schemas into a single federated graph. Returns a
breaking-change report against the previously stored composition.

**Request body**
```json
{
  "services": [
    {
      "service_name": "users",
      "types": {
        "User": ["id", "name", "email"]
      }
    },
    {
      "service_name": "billing",
      "types": {
        "Invoice": ["id", "amount", "currency"],
        "User":    ["id", "plan"]
      }
    }
  ]
}
```

**Response 200**
```json
{
  "contributing_services": ["users", "billing"],
  "types": {
    "Invoice": ["amount", "currency", "id"],
    "User":    ["email", "id", "name", "plan"]
  },
  "breaking_changes": []
}
```

`breaking_changes` is non-empty when a previously composed type/field
was removed — e.g. `["User.email removed"]`.

**Response 422** — duplicate service name or other composition conflict.

---

### `GET /api/federation/status`

Summary of the currently stored composed schema.

**Response 200**
```json
{
  "contributing_services": ["users", "billing"],
  "type_count": 2,
  "field_count": 7
}
```

---

## AI Routing Engine

### `POST /api/ai/route`

Select the best AI provider from a candidate list according to the given policy.

**Request body**
```json
{
  "policy": "cost",
  "min_context_length": 4000,
  "candidates": [
    {
      "provider": "local_llm",
      "estimated_cost_usd_per_1k_tokens": 0.0,
      "estimated_latency_ms": 900,
      "is_local": true,
      "context_length": 8000
    },
    {
      "provider": "anthropic",
      "estimated_cost_usd_per_1k_tokens": 3.0,
      "estimated_latency_ms": 400,
      "is_local": false,
      "context_length": 200000
    }
  ]
}
```

| Field                | Type    | Required | Values                                        |
|----------------------|---------|----------|-----------------------------------------------|
| `policy`             | string  | ✅       | `cost` `latency` `local` `privacy`            |
| `min_context_length` | integer | —        | token count minimum (default 0)               |
| `candidates`         | array   | ✅       | at least one candidate                        |

**Provider values:** `openai` `anthropic` `google_gemini` `deepseek` `local_llm` `custom`

**Response 200**
```json
{
  "selected_provider": "local_llm",
  "is_local": true,
  "estimated_cost_usd_per_1k_tokens": 0.0,
  "estimated_latency_ms": 900
}
```

**Response 422** — no candidate meets `min_context_length`.

---

## DUAL DATABASE

### `GET /api/db/status`

Returns the active database backend name and a liveness confirmation.

**Response 200**
```json
{
  "backend": "dual(postgres+aruaru-db)",
  "status": "ok"
}
```

> In local development / tests the backend is `"in-memory"`.

---

### `GET /api/db/routing`

Returns the per-table routing decisions: which logical tables go to PostgreSQL,
aruaru-db, or both simultaneously.

**Response 200**
```json
{
  "default_target": "postgresql",
  "entries": [
    { "table": "sessions",        "target": "postgresql" },
    { "table": "api_keys",        "target": "postgresql" },
    { "table": "rate_limits",     "target": "postgresql" },
    { "table": "schemas",         "target": "both" },
    { "table": "backup_jobs",     "target": "both" },
    { "table": "schema_history",  "target": "aruaru-db" },
    { "table": "change_records",  "target": "aruaru-db" },
    { "table": "audit_log",       "target": "aruaru-db" }
  ]
}
```

---

### `GET /api/db/:table`

List all records in a logical table.

**Path param** — `table`: logical table name (e.g. `schemas`, `audit_log`)

**Response 200**
```json
{
  "table": "schemas",
  "count": 2,
  "records": [
    { "key": "billing", "value": { "sdl": "type Invoice{}" } },
    { "key": "users",   "value": { "sdl": "type User{}" } }
  ]
}
```

---

### `GET /api/db/:table/:key`

Retrieve a single record.

**Response 200**
```json
{
  "table": "schemas",
  "key": "users",
  "value": { "sdl": "type User { id: ID! name: String }" }
}
```

**Response 404** — key not found in the table.

---

### `PUT /api/db/:table/:key`

Upsert (create or overwrite) a record.

**Request body**
```json
{ "value": { "sdl": "type User { id: ID! name: String email: String }" } }
```

The `value` field accepts any JSON.

**Response 200** — echoes back the stored record (same shape as GET).

---

### `DELETE /api/db/:table/:key`

Delete a record. Idempotent — returns success even if the key did not exist.

**Response 200**
```json
{
  "table": "schemas",
  "key": "users",
  "deleted": true
}
```

---

## Versioning Policy

Per the VersionlessAPI Engine (`docs/versionless-api.md`): new fields are
additive and old fields are deprecated (not removed) until all clients have
migrated. There is no `/v1`-style prefix and none is planned — see
`crates/open-runo-versionless-api` for the compatibility rule engine.

## Error format

Most error responses (400/404/409/422/500) return a JSON body
`{"error": "<message>"}` alongside the HTTP status code. Authentication
failures (401, missing/invalid `X-Api-Key`) return an empty body with just
the status code — check the status first, don't assume a JSON body is
always present.
