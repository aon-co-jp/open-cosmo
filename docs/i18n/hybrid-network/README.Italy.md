# Architettura di Rete Ibrida (Sommario)


**Missione (unificata in v0.2):** Consegna garantita e lettura/scrittura garantite per dati che non devono mai andare persi — oggetti a pagamento nei giochi 3D online, finanza online, titoli/brokeraggio online. Velocità e stack di trasporto a 4 livelli esistono per servire questa missione, non per competere con essa.

**Obiettivo:** Combinare uno stack di trasporto a 4 livelli (UDP grezzo → QUIC/HTTP3 → fallback TCP → multiplexing federazione GraphQL) con le garanzie ACID di `aruaru-db` e l'integrità in stile ZFS di `open-raid-z`, tra `open-runo`, `poem-cosmo-tauri`, `open-web-server`, `aruaru-db` e `open-raid-z`.

**Stato attuale:** L'integrazione Poem di `aruaru-db` è verificata veloce; la compatibilità SQL UPSERT con `open-runo` è ancora aperta. `open-raid-z` ha I/O non allineato e strumenti di migrazione funzionanti, ma i tipi nativi Windows non sono disponibili su CI Linux. `open-web-server` non è stato ancora verificato.

**Prossimi passi:** (1) correggere il parser UPSERT, (2) verificare `open-web-server`, (3) definire un contratto condiviso di negoziazione del trasporto, (4) collegare i checksum in stile ZFS al percorso di scrittura del DB, (5) costruire per ultimo il percorso veloce QUIC/UDP.

Regole complete in `docs/HYBRID_NETWORK_ARCHITECTURE.md`. Nota: redatto senza ricerca web in tempo reale; le affermazioni "stato dell'arte" vanno considerate non verificate fino al benchmark.

**Regola di ricerca:** Sviluppo e manutenzione devono cercare attivamente sul web (es. Google) e su GitHub quando necessario — e le ricerche vanno effettuate **sia in giapponese che in inglese**, poiché le informazioni rilevanti (post di blog, avvisi, issue) spesso emergono in una sola lingua.

**Aggiornamento (v0.6):** in questa sessione poem-cosmo-tauri ha chiuso diverse lacune precedentemente rimandate (streaming/reflection gRPC, upload non-Multipart, EDFS via Redis, un campo Cosmo Connect di portata limitata) e corretto due errori di documentazione obsoleti. Vedi §0.6 nel documento completo per il registro e cosa resta realmente bloccato dall'ambiente.
