# 하이브리드 네트워크 아키텍처 (요약)

**목표:** 4계층 전송 스택(순수 UDP → QUIC/HTTP3 → TCP 폴백 → GraphQL 페더레이션 멀티플렉싱)과 `aruaru-db`의 ACID 보장, `open-raid-z`의 ZFS 스타일 무결성을 `open-runo`, `poem-cosmo-tauri`, `open-web-server`, `aruaru-db`, `open-raid-z` 전반에 걸쳐 결합합니다.

**현재 상태:** `aruaru-db`의 Poem 통합은 고성능이 확인됨. `open-runo`와의 SQL UPSERT 호환성은 아직 미해결. `open-raid-z`는 비정렬 I/O와 마이그레이션 도구가 동작하지만, Linux CI에서는 Windows 전용 타입을 사용할 수 없음. `open-web-server`는 아직 감사되지 않음.

**다음 단계:** (1) UPSERT 파서 문제 해결, (2) `open-web-server` 감사, (3) 공유 전송 협상 계약 정의, (4) DB 쓰기 경로에 ZFS 스타일 체크섬 연결, (5) QUIC/UDP 고속 경로는 마지막에 구축.

자세한 내용은 `docs/HYBRID_NETWORK_ARCHITECTURE.md` 참조. 참고: 실시간 웹 조사 없이 작성되었으므로 "최첨단" 관련 주장은 벤치마크 전까지 미검증으로 취급할 것.
