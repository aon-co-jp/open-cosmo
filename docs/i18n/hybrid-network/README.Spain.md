# Arquitectura de Red Híbrida (Resumen)

**Objetivo:** Combinar una pila de transporte de 4 capas (UDP crudo → QUIC/HTTP3 → repliegue TCP → multiplexación de federación GraphQL) con las garantías ACID de `aruaru-db` y la integridad estilo ZFS de `open-raid-z`, en `open-runo`, `poem-cosmo-tauri`, `open-web-server`, `aruaru-db` y `open-raid-z`.

**Estado actual:** La integración Poem de `aruaru-db` está verificada como rápida; la compatibilidad SQL UPSERT con `open-runo` sigue pendiente. `open-raid-z` tiene E/S no alineada y herramientas de migración funcionales, pero los tipos nativos de Windows no están disponibles en CI de Linux. `open-web-server` aún no ha sido auditado.

**Próximos pasos:** (1) corregir el parser de UPSERT, (2) auditar `open-web-server`, (3) definir un contrato compartido de negociación de transporte, (4) conectar sumas de verificación estilo ZFS a la ruta de escritura de la base de datos, (5) construir la ruta rápida QUIC/UDP al final.

Reglas completas en `docs/HYBRID_NETWORK_ARCHITECTURE.md`. Nota: redactado sin búsqueda web en vivo; las afirmaciones de "vanguardia" deben tratarse como no verificadas hasta el benchmark.
