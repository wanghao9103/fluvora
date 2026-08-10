# Production v1 验收门禁

[简体中文](PRODUCTION_ACCEPTANCE.md) | [English](en/PRODUCTION_ACCEPTANCE.md)

Fluvora 只有在下列自动门禁和环境认证同时满足时才能发布。代码存在不等于生产环境认证完成。

## 自动门禁

| 门禁 | 执行位置 | 通过条件 |
|---|---|---|
| 架构/文档 | CI + Release | crate 依赖向内、模块预算合规、必需文档和本地链接完整 |
| Rust format/clippy/tests | CI + Release | workspace 全绿，`-D warnings` |
| 供应链 | CI + Release | cargo-deny、SBOM、provenance、Cosign 签名 |
| SDK 契约 | CI + Release | Web/Rust/C/Android/Swift 公共操作与常量一致 |
| TURN 数据面 | CI + Release | 真实服务的 UDP/TCP/TLS 均完成认证、Allocate、Permission、Send/Data、ChannelBind/ChannelData 和释放 |
| 浏览器 SFU | CI + Release | Chromium/Firefox/WebKit 的 ICE/DTLS/SCTP/DataChannel 与双端 VP8 SRTP 转发 PASS |
| 浏览器 P2P | CI + Release | 双参与者信令、直连、DataChannel 消息和端到端视频 RTP PASS |
| 弱网 | CI + Release | 80±20 ms、5% loss、1% reorder 下 Chromium SFU/P2P PASS |
| 直播/点播管线 | CI + Release | 真实 FFmpeg 点播输入与直播 VP8/RTP 均生成双档、可读、相对 URI 的 fMP4/HLS master/rendition |
| 媒体热路径 | CI + nightly | release 模式达到输出包速率和 p99 阈值 |
| 控制面 | browser CI + nightly | 事务业务流零错误且 p95 在阈值内 |
| 协议 fuzz | nightly | STUN/RTP/DataChannel 三目标限时无 crash |
| PostgreSQL/NATS | CI | 事务、outbox、幂等、lease/fencing、JetStream PASS |
| Android/Swift | CI + Release | 单元测试和 release build PASS |

`scripts/run-browser-interop.sh` 启动真实 Rust API/media-node，运行控制面 quick load 和
Playwright 三浏览器矩阵；DataChannel 用例同时打开可靠有序和 `maxRetransmits=0` 部分可靠
通道。Windows 开发机使用 `scripts/run-browser-interop.ps1` 运行已安装的 Playwright
浏览器及可选短时 soak。Linux 设置 `FLUVORA_NETEM=true` 注入弱网；设置
`FLUVORA_SOAK_SECONDS`、`FLUVORA_SOAK_CONCURRENCY` 和 `FLUVORA_SKIP_BROWSER=true`
可运行长稳控制面负载。soak 模式默认在 token TTL 的三分之一处重新签发，并以原子 rename
替换 token 文件；控制面与媒体处理直方图必须产生非零观测值，否则脚本失败。

`scripts/smoke-hls-pipelines.ps1` 启动真实 media-worker：先把带音视频的 MP4 打包为点播
fMP4/HLS，再将实时 VP8/RTP 输入打包为滚动直播 HLS。门禁验证 master/rendition、init
segment、media segment、FFprobe 可读性、任务终态，以及清单中不存在本机绝对路径。

`fluvora-turn-probe` 可连接已部署 TURN，并为 UDP/TCP/TLS 分别验证认证挑战、响应完整性、
relay 地址、双向 Send/Data indication、ChannelData 和 allocation 释放。`scripts/smoke-turn.ps1`
对本机真实进程执行三传输门禁；公网认证在独立节点运行 `echo` 模式，并通过 `--peer` 指向它。
生产凭据使用 `--password-file` 或 `FLUVORA_TURN_PROBE_PASSWORD` 注入，探针 JSON 不记录密码。

`scripts/run-release-gates.ps1 -Profile quick|full` 汇总本机门禁，并在 `artifacts/` 写入版本化
JSON、逐项日志和 TURN 证据。失败的门禁也会写入结果，不能用缺失证据替代 PASS。

`fluvora-perf-lab` 的容量档默认路由 100,000 个 1000 字节 RTP 输入到 64 个订阅者，并验证
输出数量、吞吐与输入处理 p99。`scripts/load-control-plane.mjs --profile soak` 默认配置为
48 小时。业务 token 仍保持最多 24 小时：外部认证环境通过 `--token-file` 提供由安全签发器
原子更新的文件，压测器定时热加载，并在 401 时强制重载一次；不要为压测放宽 token 安全上限。

## 环境认证

以下项目依赖真实基础设施，不能由单机测试替代：

- TURN UDP/TCP/TLS 经公网 NAT、防火墙和企业代理互通，三种传输均留存探针 JSON；
- 1/10/50/200/1000 人容量与音频优先 SLO；
- 48 小时混合直播、点播、P2P、SFU、聊天/礼物 soak；
- PostgreSQL PITR、对象存储版本恢复和区域级灾备；
- worker/media-node crash、网络分区、磁盘满、证书过期和告警演练；
- 1% → 10% → 50% → 100% 灰度及不可变 digest 回滚；
- 第三方安全审计无阻断问题，责任人、SLO、RTO/RPO 和支持流程签字。

每次 Production v1 发布把环境认证证据链接到 release 记录；任何一项过期或失败都必须阻断发布。
