# open-runo

**순수 Rust로 만든 GraphQL Federation 플랫폼**(Poem/Tauri/Cosmo는 직접
의존성이 아니며, tokio+hyper 위에서 호환성을 유지하도록 기능을 직접 재구현함)
— WunderGraph Cosmo 유료 플랜 기능을 OSS로 제공. 자체 학습 AI 내장(외부 LLM 계약 불필요).

📖 다른 언어: [日本語](README-Japan.md) / [English](README-English.md) ·
다른 프로젝트 연동은 **[PORTING.md](PORTING.md)** 참고.

## open-runo란?

마이크로서비스가 늘어나면서 REST API가 난립(BFF 지옥, `/v1 /v2` 버전 폭발,
엔드포인트 관리 붕괴)합니다. open-runo는 **GraphQL Federation + VersionlessAPI**로
이 문제를 근본적으로 해결합니다. Go로 만들어진 WunderGraph Cosmo가 유료 플랜
(Launch/Scale/Enterprise)에서만 제공하는 기능을 Pure Rust로 **전부 무료 OSS**로 구현했습니다.

## 기능 비교

| 기능 | Cosmo 무료 | Cosmo 유료 | **open-runo** |
|------|:---:|:---:|:---:|
| GraphQL Federation / Schema Registry | ✅ | ✅ | ✅ |
| Persisted Queries | — | ✅ | ✅ **무료** |
| 세밀한 RBAC | — | ✅ | ✅ **무료** |
| SSO(OIDC/JWKS RS256) | — | ✅ | ✅ **무료** |
| SCIM 2.0 프로비저닝 | — | ✅ | ✅ **무료** |
| 감사 로그 | — | ✅ | ✅ **무료** |
| 요청 수/팀 인원/보존 기간 제한 | 있음 | 완화 | **전혀 없음** |

### open-runo만의 기능

- 🧠 자체 학습 AI(외부 LLM 비용 없음) — HTML 캐시 자동 판정, 렌더링 비용 학습, 적응형 TTL
- 🔑 KeyGuardian — API 키 완전 자동 운영(SCIM 연동 자동 발급/해지, 이상 사용 자동 격리)
- 🗄️ DUAL DATABASE — PostgreSQL + aruaru-db 이중화, 정합성 자동 검증/복구
- 📦 간편 이전/복구 — 전체 데이터 + AI 학습 상태를 단일 이식 가능 JSON으로
- 🔀 엔진 변환·분산 통합 — MySQL→PostgreSQL→CockroachDB 원클릭 변환
- ⚡ VersionlessAPI — `/v1 /v2` 없이 호환성 규칙 엔진으로 대응
- 🖥️ 데스크톱 관리 앱(Rust를 WebAssembly로 컴파일, Tauri/Node.js/TypeScript 미사용)
- ⌨️ CLI(`open-runo-cli`) — `wgc`에 상당하는 CLI로 스키마 등록/조회,
  federation 상태 확인, OpenAPI 스펙 조회 가능

## 빠른 시작

```bash
git clone https://github.com/aon-co-jp/open-runo
cd open-runo
cargo test --workspace          # 테스트 343개(--all-features 시 362개)
cargo run -p open-runo-gateway  # REST + GraphQL 서버 실행
```

## 워크스페이스 구성(18개 crate)

`open-runo-router`(REST 게이트웨이/인증/감사), `open-runo-gateway`(GraphQL 엔드포인트),
`open-runo-federation`(스키마 합성), `open-runo-db`(멀티 엔진 DB 추상화) 등으로 구성됩니다.
자세한 내용은 [docs/architecture.md](docs/architecture.md) 참고.

## 관련 프로젝트

`open-web-server`를 중심으로 이 저장소·`poem-cosmo-tauri`·PostgreSQL·
`aruaru-db`·`open-raid-z`를 결합한 목표 아키텍처(통신·DB 쓰기 4중화,
2026-07-11 개정)가 있으며, 3D 온라인 게임의 유료 아이템 및 금융/증권
데이터 손실을 방지하는 것이 목적이다. open-runo는 그 안에서 Federation
Gateway/백엔드로 관여할 수 있다(자세한 내용은
[open-web-server](https://github.com/aon-co-jp/open-web-server)의
`README.md`/`CLAUDE.md` 참조).

## License

Apache-2.0 OR MIT(원하는 쪽 선택). 기여는 [CONTRIBUTING.md](CONTRIBUTING.md) 참고.
