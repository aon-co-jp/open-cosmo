# open-runo

**GraphQL-Federation-Plattform, gebaut in purem Rust** (Poem/Tauri/Cosmo sind
nie direkte Abhängigkeiten — ihre Funktionalität ist für Kompatibilität auf
tokio+hyper von Hand nachgebaut)
— Kostenpflichtige Funktionen von WunderGraph Cosmo als Open Source.
Mit eigener selbstlernender KI (kein externer LLM-Vertrag nötig).

📖 Weitere Sprachen: [日本語](README-Japan.md) / [English](README-English.md) ·
Integration in andere Projekte siehe **[PORTING.md](PORTING.md)**.

## Was ist open-runo?

Mit wachsender Microservice-Landschaft wuchern REST-APIs (BFF-Hölle,
Versionsexplosion `/v1 /v2`, unkontrollierbares Endpoint-Management).
open-runo löst dies grundlegend mit **GraphQL Federation + VersionlessAPI**.
Funktionen, die das in Go geschriebene WunderGraph Cosmo nur in
Bezahlplänen (Launch/Scale/Enterprise) bietet, sind hier **vollständig in
purem Rust, kostenlos und als OSS** implementiert.

## Funktionsvergleich

| Funktion | Cosmo Free | Cosmo Paid | **open-runo** |
|---|:---:|:---:|:---:|
| GraphQL Federation / Schema Registry | ✅ | ✅ | ✅ |
| Persisted Queries | — | ✅ | ✅ **kostenlos** |
| Feingranulares RBAC | — | ✅ | ✅ **kostenlos** |
| SSO (OIDC / JWKS RS256) | — | ✅ | ✅ **kostenlos** |
| SCIM-2.0-Provisioning | — | ✅ | ✅ **kostenlos** |
| Audit-Log | — | ✅ | ✅ **kostenlos** |
| Limits für Requests/Team/Aufbewahrung | Ja | Gelockert | **Keine** |

### Nur bei open-runo

- 🧠 Selbstlernende KI (keine externen LLM-Kosten) — adaptiver HTML-Cache, dynamisches TTL
- 🔑 KeyGuardian — vollautomatische API-Key-Verwaltung (Ausgabe/Widerruf via SCIM)
- 🗄️ DUAL DATABASE — PostgreSQL + aruaru-db gespiegelt, automatische Konsistenzprüfung/-reparatur
- 📦 Einfacher Umzug/Wiederherstellung — alle Daten + KI-Lernstand in einer portablen JSON-Datei
- 🔀 Engine-Konvertierung & verteilte Integration — MySQL→PostgreSQL→CockroachDB per Befehl
- ⚡ VersionlessAPI — kein `/v1 /v2` mehr dank Kompatibilitäts-Regelengine
- 🖥️ Desktop-App, von Rust nach WebAssembly kompiliert (ohne Tauri, ohne Node.js, ohne TypeScript)
- ⌨️ CLI (`open-runo-cli`), ein `wgc`-Äquivalent für Schema-Registrierung/-Abfrage,
  Federation-Status und OpenAPI-Spec-Abruf

## Schnellstart

```bash
git clone https://github.com/aon-co-jp/open-runo
cd open-runo
cargo test --workspace          # 343 Tests (362 mit --all-features)
cargo run -p open-runo-gateway  # REST + GraphQL Server
```

## Workspace-Struktur (18 Crates)

Bestehend aus `open-runo-router` (REST-Gateway/Auth/Audit),
`open-runo-gateway` (GraphQL-Endpoint), `open-runo-federation` (Schema-Komposition),
`open-runo-db` (Multi-Engine-Abstraktion) u.a. Details: [docs/architecture.md](docs/architecture.md).

## Verwandte Projekte

Es gibt eine Zielarchitektur, die `open-web-server` mit diesem Repository,
`poem-cosmo-tauri`, PostgreSQL, `aruaru-db` und `open-raid-z` kombiniert
(vierfach redundanter Transport und vierfach redundante DB-Schreibvorgänge,
überarbeitet 2026-07-11), um den Verlust von Bezahlobjekt- sowie Finanz-/
Wertpapierdaten in 3D-Onlinespielen zu verhindern. Details siehe
`README.md`/`CLAUDE.md` von
[open-web-server](https://github.com/aon-co-jp/open-web-server).

## Lizenz

Apache-2.0 OR MIT (nach Wahl). Für Beiträge siehe [CONTRIBUTING.md](CONTRIBUTING.md).
