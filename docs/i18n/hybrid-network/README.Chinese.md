# 混合网络架构(摘要)


**使命(v0.2合并):** 对绝不能丢失的数据实现有保证的传递与读写——涵盖3D网络游戏付费道具、网络金融、网络证券。速度与四层传输是为此使命服务的,而非与之竞争。

**目标:** 将四层传输栈(原始UDP → QUIC/HTTP3 → TCP回退 → GraphQL联邦多路复用)与 `aruaru-db` 的ACID保证、`open-raid-z` 的ZFS风格完整性相结合,覆盖 `open-runo`、`poem-cosmo-tauri`、`open-web-server`、`aruaru-db`、`open-raid-z` 各项目。

**现状:** `aruaru-db` 的Poem集成已验证高性能;与 `open-runo` 的SQL UPSERT兼容性仍是待解决问题。`open-raid-z` 已实现非对齐I/O与迁移工具,但在Linux CI上无法使用Windows原生类型。`open-web-server` 尚未审计。

**后续步骤:** (1) 修复UPSERT解析器问题,(2) 审计 `open-web-server`,(3) 定义共享传输协商契约,(4) 将ZFS风格校验和接入数据库写入路径,(5) 最后再构建QUIC/UDP快速路径。

详见 `docs/HYBRID_NETWORK_ARCHITECTURE.md`。注意:本文档并非基于实时网络调研撰写,"最先进"相关说法在完成基准测试前应视为未经验证。

**调研规则:** 开发与维护应在需要时主动使用谷歌搜索和GitHub调研 —— 且应**同时用日语和英语**进行搜索,因为相关信息(博客文章、安全公告、issue)往往只出现在其中一种语言中。

**更新(v0.6):** 本次会话中 poem-cosmo-tauri 解决了多项此前搁置的问题(gRPC流式/反射、非Multipart上传、通过Redis实现EDFS、范围有限的Cosmo Connect字段),并修正了两处过时的文档错误。详情及因环境限制而真正搁置的项目请参见正文§0.6。

**更新(v0.7):** aruaru-db 现已实现与 open-raid-z 算法字节级相同的 ZFS 兼容校验和层,并与现有 ACID 事务实现了混合——每次写入都会计算校验和,每次读取都会验证,还有相当于 zpool scrub 的方法可查找所有损坏的行。详情及向其他仓库推广的步骤见 §0.7。
