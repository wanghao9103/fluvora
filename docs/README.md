# Fluvora 文档索引

[简体中文](README.md) | [English](en/README.md)

本文是仓库文档入口。设计、接口、运行和验收信息按职责拆分；同一事实只保留一个主要来源，其他
文档通过链接引用，避免多处描述逐渐不一致。

## 推荐阅读路径

### 第一次进入代码库

1. [代码库总览](CODEBASE.md)：目录、crate 职责和代码放置规则；
2. [总体架构](ARCHITECTURE.md)：运行拓扑、媒体模式、安全与恢复；
3. [代码分层](LAYERS.md)：workspace 依赖方向和门禁；
4. [API 服务内部设计](API_SERVER_STRUCTURE.md)：控制面模块、调用链和状态所有权。

### 接入 SDK 或公开 API

1. [SDK 接入指南](SDK_INTEGRATION.md)：五端安装、鉴权、媒体、错误、释放和排障；
2. [公开 API](API.md)：HTTP、WebSocket、WHIP/WHEP 和媒体控制接口；
3. [SDK 示例](SDK_DEMOS.md)：各平台可运行示例与能力覆盖；
4. [SDK 独立版本与发布](SDK_RELEASES.md)：版本来源、Changelog 边界、标签和候选包；
5. `sdk-contract-v1.json`：跨端操作和安全边界的机器可检验契约；
6. `sdk-demo-contract-v1.json`：示例覆盖的机器可检验契约。

### 部署与上线

1. [生产验收](PRODUCTION_ACCEPTANCE.md)：上线前门禁和证据；
2. [运维手册](RUNBOOK.md)：启动、观测、故障定位和处置；
3. [开发计划](DEVELOPMENT_PLAN.md)：里程碑、角色和交付节奏。

## 文档职责

| 文档 | 主要回答的问题 | 更新触发条件 |
|---|---|---|
| `ARCHITECTURE.md` | 系统由哪些进程组成，控制面与媒体面如何协作 | 服务拓扑、协议边界、数据所有权变化 |
| `LAYERS.md` | crate 可以依赖谁，代码应向哪一层移动 | workspace crate 或依赖层级变化 |
| `CODEBASE.md` | 文件在哪里，新代码放在哪里 | 目录、crate、主要模块变化 |
| `API_SERVER_STRUCTURE.md` | API 服务内部如何分层、并发和持久化 | API 内部职责、调用链、门禁变化 |
| `API.md` | 客户端可调用哪些公开接口 | 路由、字段、状态码、限制变化 |
| `SDK_INTEGRATION.md` | 五端如何安装、连接媒体、处理错误和释放 | SDK 构造器、公开方法、平台边界变化 |
| `SDK_DEMOS.md` | 各 SDK 如何使用能力 | SDK 公开方法或示例变化 |
| `SDK_RELEASES.md` | SDK 如何独立升版、构建和维护变更记录 | SDK 版本、标签或发布流程变化 |
| `PRODUCTION_ACCEPTANCE.md` | 上线必须通过哪些检查 | 发布门禁、容量和安全要求变化 |
| `RUNBOOK.md` | 线上异常如何发现和处理 | 配置、指标、告警和恢复方式变化 |
| `DEVELOPMENT_PLAN.md` | 项目如何分阶段交付 | 里程碑、人员和范围变化 |

## 信息来源优先级

发生描述冲突时按以下顺序处理：

1. 可执行契约、数据库 migration 和源码中的实际边界；
2. `API.md`、`ARCHITECTURE.md` 和专项设计文档；
3. `CODEBASE.md`、README 和示例说明；
4. 开发计划中的目标性描述。

发现冲突不应只修改较低优先级文档：先确认实际设计意图，再同步代码、契约和所有受影响文档。

## 文档维护规则

- 新增公开路由时同步 `API.md`、SDK contract 和对应 SDK；
- 新增服务或 workspace crate 时同步 `ARCHITECTURE.md`、`LAYERS.md` 和 `CODEBASE.md`；
- 调整 API 内部分层时同步 `API_SERVER_STRUCTURE.md` 和架构门禁；
- 修改配置、端口、指标或恢复流程时同步 `RUNBOOK.md`；
- 中文和英文文档必须在同一次变更中同步更新，并保留页首双向语言切换；
- 每次 full release gate 的结果以 `artifacts/release-gates-*/release-gates.json` 为证据；
- Markdown 使用 UTF-8、LF 和仓库 `.editorconfig`，链接使用仓库相对路径。

文档修改完成后至少运行：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/check-docs.ps1
```
