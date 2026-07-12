# Architecture Réseau Hybride (Résumé)

**Objectif :** Combiner une pile de transport à 4 couches (UDP brut → QUIC/HTTP3 → repli TCP → multiplexage fédération GraphQL) avec les garanties ACID d'`aruaru-db` et l'intégrité de style ZFS d'`open-raid-z`, sur `open-runo`, `poem-cosmo-tauri`, `open-web-server`, `aruaru-db` et `open-raid-z`.

**État actuel :** L'intégration Poem d'`aruaru-db` est confirmée performante ; la compatibilité SQL UPSERT avec `open-runo` reste un point ouvert. `open-raid-z` dispose d'E/S non alignées et d'un outil de migration fonctionnels, mais les types natifs Windows sont indisponibles sous CI Linux. `open-web-server` n'a pas encore été audité.

**Prochaines étapes :** (1) corriger le parseur UPSERT, (2) auditer `open-web-server`, (3) définir un contrat de négociation de transport partagé, (4) relier les sommes de contrôle façon ZFS au chemin d'écriture de la base, (5) construire la voie rapide QUIC/UDP en dernier.

Voir `docs/HYBRID_NETWORK_ARCHITECTURE.md` pour les règles complètes. Remarque : rédigé sans recherche web en temps réel ; les affirmations « à la pointe » doivent être considérées non vérifiées avant tout benchmark.
