# Hybride Netzwerkarchitektur (Zusammenfassung)


**Mission (in v0.2 zusammengeführt):** Garantierte Zustellung und garantiertes Lesen/Schreiben für Daten, die niemals verloren gehen dürfen — kostenpflichtige Items in 3D-Online-Spielen, Online-Finanzen, Online-Wertpapiere/Brokerage. Geschwindigkeit und die 4-Schichten-Transportstrategie dienen dieser Mission, statt mit ihr zu konkurrieren.

**Ziel:** Ein 4-Schichten-Transport-Stack (rohes UDP → QUIC/HTTP3 → TCP-Fallback → GraphQL-Federation-Multiplexing) mit den ACID-Garantien von `aruaru-db` und der ZFS-artigen Integrität von `open-raid-z` kombinieren, über `open-runo`, `poem-cosmo-tauri`, `open-web-server`, `aruaru-db` und `open-raid-z` hinweg.

**Aktueller Stand:** Die Poem-Integration von `aruaru-db` ist nachweislich schnell; die SQL-UPSERT-Kompatibilität mit `open-runo` ist noch offen. `open-raid-z` verfügt über funktionierende unaligned I/O und Migrationswerkzeuge, aber Windows-native Typen sind unter Linux-CI nicht verfügbar. `open-web-server` wurde noch nicht geprüft.

**Nächste Schritte:** (1) UPSERT-Parser-Lücke schließen, (2) `open-web-server` prüfen, (3) gemeinsamen Transport-Verhandlungsvertrag definieren, (4) ZFS-artige Prüfsummen in den DB-Schreibpfad einbinden, (5) QUIC/UDP-Fastpath zuletzt bauen.

Vollständige Regeln siehe `docs/HYBRID_NETWORK_ARCHITECTURE.md`. Hinweis: ohne Live-Websuche erstellt; "State of the Art"-Aussagen gelten bis zum Benchmark als unverifiziert.

**Recherche-Regel:** Entwicklung und Wartung sollen bei Bedarf aktiv im Web (z. B. Google) und auf GitHub recherchieren — und zwar **sowohl auf Japanisch als auch auf Englisch**, da relevante Informationen (Blogbeiträge, Advisories, Issues) oft nur in einer Sprache auftauchen.

**Update (v0.6):** In dieser Sitzung hat poem-cosmo-tauri mehrere zuvor zurückgestellte Lücken geschlossen (gRPC-Streaming/-Reflection, Nicht-Multipart-Upload, EDFS via Redis, ein eingeschränktes Cosmo-Connect-Feld) und zwei veraltete Dokumentationsfehler behoben. Siehe §0.6 im vollständigen Dokument für das Protokoll und was durch die Umgebung noch wirklich blockiert ist.

**Update (v0.7):** aruaru-db verfügt nun über eine ZFS-kompatible Prüfsummen-Schicht (byteidentischer SHA-256-Algorithmus wie bei open-raid-z), hybridisiert mit den bestehenden ACID-Transaktionen -- jeder Schreibvorgang erhält eine Prüfsumme, jeder Lesevorgang wird verifiziert, und eine zpool-scrub-äquivalente Methode findet alle beschädigten Zeilen. Siehe §0.7 für Details und weitere Rollout-Schritte.

**Anweisung für die nächste Sitzung (v0.8):** die ZFS+ACID-Hybridfusion über alle Repos hinweg fortsetzen und (via Google/GitHub-Suche auf Japanisch UND Englisch, nicht nur aus dem Gedächtnis) recherchieren, dann den 4-Schichten-/4-fach-redundanten Transport-Stack (TCP-IP/UDP-IP/QUIC-MPQUIC/MPTCP-SCTP) implementieren -- als EINE fusionierte Zero-Loss-Pipeline für Spiele-Items/Finanzen/Wertpapiere/Kreditkartendaten, nicht als separate Features. Siehe §0.8.

**Update (v0.9):** die Poem/Tauri-Reproduktionsmission ist nun explizit formuliert: beide werden von Grund auf entwickelt und mit voller Kompatibilität reproduziert, einschließlich der Ausführung im Browser. Zudem wurden veraltete einseitige Lead/Mirror-Formulierungen korrigiert und die Doku-Link-Listen in den open-runo/poem-cosmo-tauri-READMEs linksbündig ausgerichtet. Hinweis: In dieser Sitzung stand kein Live-Websuche-Tool für die Forschungsanweisung in §0.8 zur Verfügung.
