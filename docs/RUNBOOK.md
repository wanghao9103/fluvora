# Fluvora Production v1 运维手册

[简体中文](RUNBOOK.md) | [English](en/RUNBOOK.md)

本文约定生产环境的发布、回滚、备份恢复和常见故障处置。所有命令都必须先在对应环境的
只读上下文中确认 namespace、集群和数据库目标；示例中的占位符不得直接执行。

## 1. 发布前检查

1. tag 对应的 Release workflow 必须全部通过；镜像必须同时具有 Cosign 签名、SLSA
   provenance 和 SPDX SBOM。
2. 确认 `FLUVORA_PUBLIC_IP`、TURN relay 端口、防火墙、API/gateway TLS ingress、
   DTLS/TURN 证书和对象存储 endpoint 均属于目标环境。
3. 确认 active/retiring token key 顺序，内部 token 彼此隔离，示例 Secret 已全部替换。
4. PostgreSQL 备份和对象存储版本化检查必须在变更窗口内成功。
5. Prometheus 的 `FluvoraServiceDown`、心跳、鉴权失败、DataChannel 重传耗尽/部分可靠放弃、TURN
   端口压力和 worker 失败告警必须处于可投递状态。

## 2. 灰度与回滚

使用不可变镜像 digest，不使用可变 tag。按 1% → 10% → 50% → 100% 扩大流量，每一阶段
至少观察两个告警窗口，并比较连接成功率、首包时间、丢包、p95/p99、转码失败和 5xx。

回滚条件：

- WebRTC/WHIP/WHEP 建连成功率下降超过 1%；
- 5xx、鉴权失败或 DataChannel delivery failure 连续两个窗口异常；
- PostgreSQL outbox backlog、worker queue 或 TURN 端口使用率持续增长；
- 新版本产生不可逆数据写入但迁移没有兼容的 down/forward-fix 路径。

回滚时把 Deployment 镜像恢复到上一个已签名 digest。API/dispatcher 可先回滚；media-node
先标记 draining，停止新 placement，等待现有房间结束后再回滚。数据库迁移保持向后兼容，
不得在同一发布中删除旧列；破坏性清理必须晚至少一个版本。

## 3. PostgreSQL 备份与恢复

RPO 目标由业务确定，建议 WAL 连续归档并每日至少一次逻辑/物理全量备份。逻辑备份示例：

```bash
pg_dump --format=custom --no-owner --no-acl \
  --dbname="$FLUVORA_DATABASE_URL" --file="fluvora-$(date -u +%Y%m%dT%H%M%SZ).dump"
pg_restore --list fluvora-YYYYMMDDTHHMMSSZ.dump >/dev/null
```

恢复演练必须使用隔离数据库：

```bash
createdb fluvora_restore_drill
pg_restore --exit-on-error --clean --if-exists --no-owner --no-acl \
  --dbname=fluvora_restore_drill fluvora-YYYYMMDDTHHMMSSZ.dump
```

恢复后先运行迁移和只读一致性检查，再切换 API。检查房间版本、礼物 ledger、outbox、
token revocation、service lease/placement generation 和 signal sequence；旧 owner 的
fencing generation 不能重新获得写权限。

event-dispatcher 每分钟分批清理已成功投递且超过保留期的 outbox 行；默认保留 168 小时，
由 `FLUVORA_OUTBOX_RETENTION_HOURS`（1–8760）调整，每批上限由
`FLUVORA_OUTBOX_CLEANUP_BATCH`（1–10000）调整。清理不会选择 pending 或 leased 行，
指标 `fluvora_event_dispatcher_pruned_total` 用于确认历史积压是否持续下降。修改保留期前应先
核对 JetStream `max_age`、审计要求和数据库备份策略。

未配置 PostgreSQL 的单机开发模式使用 `FLUVORA_STATE_DIR` 房间快照，每个房间保留最新两个版本。
写入先落同目录临时文件并同步，再原子 rename；同 revision 的相同内容允许幂等重试，不同
内容会拒绝。快照保存经过校验的当前聚合状态，不复制历史聊天或自定义 payload；旧版完整事件流
快照仍可向后读取。启动时逐版本校验快照文件名、内部 room/revision、创建命令和有界幂等历史；
最新快照损坏、不可读或聚合状态非法时回退到上一个有效版本。持久化 ID 写为 32 位
十六进制字符串，恢复器仍可读取旧的 `u64` 数值 ID。该模式不用于多副本生产部署。

media-gateway 的 asset/live 元数据和 media-worker 的 assignment fence 同样保留最新两个
有效版本。写入先落同目录临时文件并同步，再原子 rename；启动时校验文件名 revision、内部
identity、领域状态、任务边界和 worker endpoint。发现损坏、伪造或超限快照时记录日志并回退，
不要手工把损坏文件改名成更高 revision。

## 4. 对象存储恢复

- 生产 bucket 必须启用版本化、服务端加密、生命周期和删除保护。
- 数据库中的 asset/live 元数据与对象发布 marker 一起恢复，不能只恢复 manifest。
- HLS 恢复后逐个验证 init segment、所有 manifest 引用和 checksum；缺段资产保持 failed，
  不发布半完整 playlist。
- 保留策略由 `FLUVORA_VOD_RETENTION_HOURS` 和 `FLUVORA_LIVE_RETENTION_HOURS` 控制；
  法务保留对象必须使用独立 prefix/bucket policy，不能依赖应用层过期时间。

## 5. 故障处置

### media-node 或 worker 退出

确认心跳过期、placement generation 已推进且旧实例被 fencing。新 worker 重建实时转码时
观察 `media.transcode_restarted`；连续三次 probe 失败或 failover rejected 时停止自动重试，
保存任务/placement 证据并扩容健康池。失败尝试的清理必须携带 generation；旧 generation
清理返回未删除属于预期结果，不能改成无条件删除当前 placement。

### media-gateway 代理异常

API 对 gateway 连接失败、重定向、5xx、超过 1 MiB 或非 JSON 的控制响应返回 502。先检查
gateway readiness、`FLUVORA_GATEWAY_URL` 与内部 token，再核对 ingress/service 是否误加
重定向或 HTML 错误页；不要临时放宽代理响应类型或启用自动重定向。

### PostgreSQL 或 NATS 不可用

API readiness 会失败。不要绕过 readiness 强行接流量。PostgreSQL 恢复后核对 outbox backlog；
NATS 恢复后 dispatcher 依赖 durable outbox 重放，按 event id 去重。禁止手工跳过未确认事件。

### 磁盘满或对象存储失败

停止新的 upload/live/transcode admission，保留读取流量。清理只允许通过生命周期或已确认的
deleted 资产，不能直接删除共享 media root。恢复容量后校验临时文件、multipart upload 和
原子 publication marker。

### TURN 端口压力

达到 80% 时扩容/分片 TURN 节点并扩大防火墙与配置一致的 relay 端口池。不得复用仍有
allocation 的端口。检查异常来源 IP、nonce/鉴权失败和 TCP/TLS fallback 比例。
从故障网络运行 `fluvora-turn-probe`，依次验证 UDP、TCP、TLS；外部 echo 必须位于 TURN
节点之外，才能证明公网 relay 路径而不只是本机回环。凭据通过文件或密钥环境变量注入。

### 证书即将过期

先部署新证书并让 API SDP fingerprint 与 media-node 身份同步，再排空旧节点。TURN/TLS
证书通过正式 CA 更新。监控证书剩余时间；fingerprint 不一致时立即停止新会话 placement。

## 6. 演练频率与证据

- 每日：CI/nightly、30 分钟 PostgreSQL soak、协议 fuzz smoke、容量基准；
- 每周：worker/media-node crash、NATS 重连、token rotation/revocation；
- 每月：PostgreSQL 恢复、对象存储版本恢复、证书轮换、告警通知；
- 每季度：区域级灾备、灰度回滚、48 小时混合业务 soak。

演练记录至少包含版本/digest、配置摘要、起止时间、负载、故障注入、指标、RTO/RPO、
恢复动作、残留风险和负责人。没有可追溯证据的演练不计入 Production v1 验收。
本机/候选版本门禁使用 `scripts/run-release-gates.ps1 -Profile full` 生成基础证据；公网
TURN 的 UDP/TCP/TLS JSON、48 小时 soak 摘要和灾备记录追加到同一 release 证据目录。
