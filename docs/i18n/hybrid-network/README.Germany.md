# Hybride Netzwerkarchitektur (Zusammenfassung)


**Mission (in v0.2 zusammengeführt):** Garantierte Zustellung und garantiertes Lesen/Schreiben für Daten, die niemals verloren gehen dürfen — kostenpflichtige Items in 3D-Online-Spielen, Online-Finanzen, Online-Wertpapiere/Brokerage. Geschwindigkeit und die 4-Schichten-Transportstrategie dienen dieser Mission, statt mit ihr zu konkurrieren.

**Ziel:** Ein 4-Schichten-Transport-Stack (rohes UDP → QUIC/HTTP3 → TCP-Fallback → GraphQL-Federation-Multiplexing) mit den ACID-Garantien von `aruaru-db` und der ZFS-artigen Integrität von `open-raid-z` kombinieren, über `open-runo`, `poem-cosmo-tauri`, `open-web-server`, `aruaru-db` und `open-raid-z` hinweg.

**Aktueller Stand:** Die Poem-Integration von `aruaru-db` ist nachweislich schnell; die SQL-UPSERT-Kompatibilität mit `open-runo` ist noch offen. `open-raid-z` verfügt über funktionierende unaligned I/O und Migrationswerkzeuge, aber Windows-native Typen sind unter Linux-CI nicht verfügbar. `open-web-server` wurde noch nicht geprüft.

**Nächste Schritte:** (1) UPSERT-Parser-Lücke schließen, (2) `open-web-server` prüfen, (3) gemeinsamen Transport-Verhandlungsvertrag definieren, (4) ZFS-artige Prüfsummen in den DB-Schreibpfad einbinden, (5) QUIC/UDP-Fastpath zuletzt bauen.

Vollständige Regeln siehe `docs/HYBRID_NETWORK_ARCHITECTURE.md`. Hinweis: ohne Live-Websuche erstellt; "State of the Art"-Aussagen gelten bis zum Benchmark als unverifiziert.
