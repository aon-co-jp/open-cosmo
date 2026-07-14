# open-runo

**Plateforme GraphQL Federation en Rust pur** (Poem/Tauri/Cosmo ne sont jamais
des dépendances directes — leurs fonctionnalités sont réimplémentées à la main
pour rester compatibles au-dessus de tokio+hyper)
— Les fonctionnalités payantes de WunderGraph Cosmo, en open source.
IA auto-apprenante intégrée (aucun contrat LLM externe requis).

📖 Autres langues : [日本語](README-Japan.md) / [English](README-English.md) ·
Pour l'intégration dans d'autres projets, voir **[PORTING.md](PORTING.md)**.

## Qu'est-ce que open-runo ?

La multiplication des microservices entraîne une prolifération des API REST
(enfer BFF, explosion des versions `/v1 /v2`, gestion des endpoints hors de contrôle).
open-runo règle ce problème à la racine grâce à **GraphQL Federation + VersionlessAPI**.
Les fonctionnalités que WunderGraph Cosmo (Go) réserve à ses offres payantes
(Launch/Scale/Enterprise) sont ici **entièrement implémentées en Rust pur, gratuitement, en OSS**.

## Comparatif des fonctionnalités

| Fonctionnalité | Cosmo gratuit | Cosmo payant | **open-runo** |
|---|:---:|:---:|:---:|
| GraphQL Federation / Schema Registry | ✅ | ✅ | ✅ |
| Persisted Queries | — | ✅ | ✅ **gratuit** |
| RBAC fin | — | ✅ | ✅ **gratuit** |
| SSO (OIDC / JWKS RS256) | — | ✅ | ✅ **gratuit** |
| Provisioning SCIM 2.0 | — | ✅ | ✅ **gratuit** |
| Journal d'audit | — | ✅ | ✅ **gratuit** |
| Limites requêtes/équipe/rétention | Oui | Assouplies | **Aucune** |

### Fonctionnalités exclusives à open-runo

- 🧠 IA auto-apprenante (aucun coût de LLM externe) — cache HTML adaptatif, TTL dynamique
- 🔑 KeyGuardian — gestion entièrement automatisée des clés API (émission/révocation via SCIM)
- 🗄️ DUAL DATABASE — PostgreSQL + aruaru-db en miroir, vérification et réparation automatiques
- 📦 Migration et restauration simplifiées — toutes les données + l'état de l'IA en un seul JSON portable
- 🔀 Conversion de moteurs et intégration distribuée — MySQL→PostgreSQL→CockroachDB en une commande
- ⚡ VersionlessAPI — évite de créer `/v1 /v2` grâce à un moteur de règles de compatibilité
- 🖥️ Application de bureau compilée de Rust vers WebAssembly (sans Tauri, sans Node.js, sans TypeScript)
- ⌨️ CLI (`open-runo-cli`), équivalent à `wgc`, pour enregistrer/consulter des
  schémas, l'état de la fédération et récupérer la spec OpenAPI

## Démarrage rapide

```bash
git clone https://github.com/aon-co-jp/open-runo
cd open-runo
cargo test --workspace          # 337 tests (356 with --all-features)
cargo run -p open-runo-gateway  # serveur REST + GraphQL
```

## Structure du workspace (18 crates)

Composé de `open-runo-router` (gateway REST/auth/audit),
`open-runo-gateway` (endpoint GraphQL), `open-runo-federation` (composition de schémas),
`open-runo-db` (abstraction multi-moteur), etc. Voir [docs/architecture.md](docs/architecture.md).

## Projets liés

Il existe une architecture cible combinant `open-web-server` avec ce
dépôt, `poem-cosmo-tauri`, PostgreSQL, `aruaru-db` et `open-raid-z`
(transport et écritures DB quadruple-redondants, révisé le 2026-07-11)
pour éviter la perte de données d'objets payants et financières/
boursières dans les jeux en ligne 3D. Voir le `README.md`/`CLAUDE.md` de
[open-web-server](https://github.com/aon-co-jp/open-web-server) pour les
détails.

## Licence

Apache-2.0 OR MIT (au choix). Pour contribuer, voir [CONTRIBUTING.md](CONTRIBUTING.md).
