# open-runo

**纯 Rust 构建的 GraphQL Federation 平台**（Poem/Tauri/Cosmo 均非直接依赖 ——
其功能均为手工重新实现，以在 tokio+hyper 之上保持兼容）
—— 以开源方式实现 WunderGraph Cosmo 付费版功能，并内置自主学习 AI（无需签约外部 LLM）。

📖 其他语言: [日本語](README-Japan.md) / [English](README-English.md) ·
接入其他项目请参见 **[PORTING.md](PORTING.md)**。

## open-runo 是什么

微服务增多导致 REST API 泛滥（BFF 地狱、`/v1 /v2` 版本爆炸、端点管理失控）。
open-runo 用 **GraphQL Federation + VersionlessAPI** 从根本上解决这些问题。
Go 编写的 WunderGraph Cosmo 仅在付费方案（Launch / Scale / Enterprise）中
提供的功能，本项目用纯 Rust **全部作为开源免费实现**。

## 功能对比

| 功能 | Cosmo 免费版 | Cosmo 付费版 | **open-runo** |
|------|:---:|:---:|:---:|
| GraphQL Federation / Schema Registry | ✅ | ✅ | ✅ |
| Persisted Queries / Trusted Documents | — | ✅ | ✅ **免费** |
| 细粒度 RBAC | — | ✅ | ✅ **免费** |
| SSO（OIDC / JWKS RS256） | — | ✅ | ✅ **免费** |
| SCIM 2.0 用户/组织供应 | — | ✅ | ✅ **免费** |
| 审计日志（Git-on-SQL 存储） | — | ✅ | ✅ **免费** |
| 请求数/团队人数/保留期限制 | 有 | 部分放宽 | **完全没有** |

### open-runo 独有功能

- 🧠 自主学习 AI（零外部 LLM 付费契约）—— HTML 页面缓存自动判定、渲染成本学习、自适应 TTL
- 🔑 KeyGuardian —— API 密钥全自动运维（与 SCIM 联动的自动签发/失效、异常使用自动隔离）
- 🗄️ DUAL DATABASE —— PostgreSQL + aruaru-db 双写、一致性自动校验与自动修复
- 📦 一键搬家/一键恢复 —— 全部数据 + AI 学习状态导出为单个可移植 JSON
- 🔀 引擎转换与分布式整合 —— MySQL→PostgreSQL→CockroachDB 一键转换
- ⚡ VersionlessAPI —— 不再新增 `/v1 /v2`，用兼容性规则引擎代替
- 🖥️ 桌面管理应用（Rust 编译为 WebAssembly，不使用 Tauri/Node.js/TypeScript）
- ⌨️ CLI（`open-runo-cli`）—— 相当于 `wgc` 的命令行工具，可注册/查询 schema、
  查看 federation 状态、获取 OpenAPI 规范

## 快速开始

```bash
git clone https://github.com/aon-co-jp/open-runo
cd open-runo
cargo test --workspace          # 343 个测试(--all-features 为 362)
cargo run -p open-runo-gateway  # 启动 REST + GraphQL 服务
```

## 工作区结构（18 个 crate）

由 `open-runo-router`（REST 网关/认证/审计）、`open-runo-gateway`（GraphQL 端点）、
`open-runo-federation`（模式合成）、`open-runo-db`（多引擎数据库抽象）等模块组成，
详见 [docs/architecture.md](docs/architecture.md)。

## 相关项目

存在一个将 `open-web-server` 与本仓库、`poem-cosmo-tauri`、PostgreSQL、
`aruaru-db`、`open-raid-z` 组合起来的目标架构(通信层与数据库写入四重
冗余,2026-07-11修订),用于防止 3D 网络游戏中付费道具及金融/证券数据
的丢失。open-runo 可作为其中的 Federation Gateway/后端参与(详见
[open-web-server](https://github.com/aon-co-jp/open-web-server) 的
`README.md`/`CLAUDE.md`)。

## License

Apache-2.0 OR MIT(任选其一)。欢迎贡献,参见 [CONTRIBUTING.md](CONTRIBUTING.md)。
