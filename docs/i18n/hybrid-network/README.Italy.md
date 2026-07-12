# Architettura di Rete Ibrida (Sommario)

**Obiettivo:** Combinare uno stack di trasporto a 4 livelli (UDP grezzo → QUIC/HTTP3 → fallback TCP → multiplexing federazione GraphQL) con le garanzie ACID di `aruaru-db` e l'integrità in stile ZFS di `open-raid-z`, tra `open-runo`, `poem-cosmo-tauri`, `open-web-server`, `aruaru-db` e `open-raid-z`.

**Stato attuale:** L'integrazione Poem di `aruaru-db` è verificata veloce; la compatibilità SQL UPSERT con `open-runo` è ancora aperta. `open-raid-z` ha I/O non allineato e strumenti di migrazione funzionanti, ma i tipi nativi Windows non sono disponibili su CI Linux. `open-web-server` non è stato ancora verificato.

**Prossimi passi:** (1) correggere il parser UPSERT, (2) verificare `open-web-server`, (3) definire un contratto condiviso di negoziazione del trasporto, (4) collegare i checksum in stile ZFS al percorso di scrittura del DB, (5) costruire per ultimo il percorso veloce QUIC/UDP.

Regole complete in `docs/HYBRID_NETWORK_ARCHITECTURE.md`. Nota: redatto senza ricerca web in tempo reale; le affermazioni "stato dell'arte" vanno considerate non verificate fino al benchmark.
