# open-runo

**Piattaforma di GraphQL Federation costruita in puro Rust** (Poem/Tauri/Cosmo
non sono mai dipendenze dirette — la loro funzionalità è reimplementata a mano
per restare compatibile sopra tokio+hyper)
— Le funzionalità a pagamento di WunderGraph Cosmo, ora open source.
Include una IA auto-apprendente proprietaria (nessun contratto LLM esterno richiesto).

📖 Altre lingue: [日本語](README-Japan.md) / [English](README-English.md) ·
Per l'integrazione in altri progetti vedi **[PORTING.md](PORTING.md)**.

## Cos'è open-runo?

La crescita dei microservizi moltiplica le API REST (inferno BFF,
esplosione di versioni `/v1 /v2`, gestione degli endpoint fuori controllo).
open-runo risolve il problema alla radice con **GraphQL Federation + VersionlessAPI**.
Le funzionalità che WunderGraph Cosmo (Go) offre solo nei piani a pagamento
(Launch/Scale/Enterprise) sono qui **implementate interamente in Rust puro, gratis, come OSS**.

## Confronto funzionalità

| Funzionalità | Cosmo free | Cosmo a pagamento | **open-runo** |
|---|:---:|:---:|:---:|
| GraphQL Federation / Schema Registry | ✅ | ✅ | ✅ |
| Persisted Queries | — | ✅ | ✅ **gratis** |
| RBAC granulare | — | ✅ | ✅ **gratis** |
| SSO (OIDC / JWKS RS256) | — | ✅ | ✅ **gratis** |
| Provisioning SCIM 2.0 | — | ✅ | ✅ **gratis** |
| Audit log | — | ✅ | ✅ **gratis** |
| Limiti richieste/team/retention | Sì | Ridotti | **Nessuno** |

### Solo su open-runo

- 🧠 IA auto-apprendente (zero costi LLM esterni) — cache HTML adattiva, TTL dinamico
- 🔑 KeyGuardian — gestione totalmente automatica delle chiavi API (emissione/revoca via SCIM)
- 🗄️ DUAL DATABASE — PostgreSQL + aruaru-db in mirroring, verifica e riparazione automatica
- 📦 Migrazione e ripristino semplici — tutti i dati + stato IA in un unico JSON portabile
- 🔀 Conversione motori e integrazione distribuita — MySQL→PostgreSQL→CockroachDB con un comando
- ⚡ VersionlessAPI — evita `/v1 /v2` grazie a un motore di regole di compatibilità
- 🖥️ App desktop compilata da Rust a WebAssembly (senza Tauri, senza Node.js, senza TypeScript)
- ⌨️ CLI (`open-runo-cli`), equivalente a `wgc`, per registrare/recuperare schemi,
  stato della federation e spec OpenAPI

## Avvio rapido

```bash
git clone https://github.com/aon-co-jp/open-runo
cd open-runo
cargo test --workspace          # 337 test (356 con --all-features)
cargo run -p open-runo-gateway  # server REST + GraphQL
```

## Struttura del workspace (18 crate)

Composto da `open-runo-router` (gateway REST/auth/audit),
`open-runo-gateway` (endpoint GraphQL), `open-runo-federation` (composizione schemi),
`open-runo-db` (astrazione multi-motore), ecc. Dettagli: [docs/architecture.md](docs/architecture.md).

## Progetti correlati

Esiste un'architettura obiettivo che combina `open-web-server` con questo
repository, `poem-cosmo-tauri`, PostgreSQL, `aruaru-db` e `open-raid-z`
(trasporto e scritture DB a quadrupla ridondanza, rivisto 2026-07-11) per
evitare la perdita di dati di oggetti a pagamento e dati finanziari/
azionari nei giochi online 3D. Dettagli in `README.md`/`CLAUDE.md` di
[open-web-server](https://github.com/aon-co-jp/open-web-server).

## Licenza

Apache-2.0 OR MIT (a scelta). Per contribuire vedi [CONTRIBUTING.md](CONTRIBUTING.md).
