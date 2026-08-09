# Fluvora 生产化开发周期

计划起点：2026-07-27  
Production v1 目标：2027-07-25  
周期：52 周，26 个双周 Sprint  
推荐核心团队：11 人

仓库当前是功能闭环的 Production Candidate v1：协议、事务控制面、分布式租约/调度、
对象存储、服务、五套 SDK、部署、监控、安全和自动发布门禁均已有实现与测试。下面的 52 周
不是“把空目录写成代码”的估算，而是完成多浏览器/多终端长期兼容、真实公网和多可用区容量、
第三方安全审计、48 小时混合媒体稳定性及运维签字，直到可以对外承诺 SLO 的认证周期。

## 1. 里程碑

| 里程碑 | 周期 | 日期 | 退出条件 |
|---|---:|---:|---|
| 技术原型认证 | Week 1-14 | 2026-11-01 | 三浏览器 ICE/DTLS/SRTP/SCTP 互通，协议 fuzz 无崩溃 |
| 实时 MVP | Week 15-26 | 2027-01-24 | P2P、50 人 SFU、弱网自适应、TURN、Web SDK、基础 SLO |
| 功能 Beta | Week 27-38 | 2027-04-18 | 直播、点播、实时转码、五套 SDK、礼物/扩展数据 |
| Release Candidate | Week 39-46 | 2027-06-13 | 分布式状态、节点调度、故障迁移、安全审计、容量基准 |
| Production v1 | Week 47-52 | 2027-07-25 | 48 小时 soak、灰度/回滚、告警演练、运维交接 |

## 2. 团队

| 角色 | 人数 | 负责范围 |
|---|---:|---|
| 架构/技术负责人 | 1 | 协议边界、ADR、质量门禁、跨组协调 |
| Rust RTC 工程师 | 3 | ICE、DTLS-SRTP、RTP/RTCP、SCTP、SFU、拥塞控制 |
| Rust 平台工程师 | 2 | API、房间、权限、状态、调度、存储 |
| 媒体管线工程师 | 1 | FFmpeg、CMAF/HLS、录制、点播、转码 |
| SDK 工程师 | 2 | Web、Rust/C、Android、iOS、兼容矩阵 |
| QA/性能工程师 | 1 | 浏览器 E2E、弱网、容量、回归、fuzz |
| SRE/安全工程师 | 1 | CI/CD、监控、证书、告警、漏洞与发布 |

少于 7 人时建议把 Production v1 延长到 16-18 个月；1-3 人适合作为研究或内部参考实现，
不应承诺公网大规模 SLA。

## 3. Sprint 计划

### Sprint 1-3：协议基线

- 固化 STUN/ICE-lite/SDP/RTP/RTCP/SRTP wire corpus；
- 对 Chrome、Firefox、Safari 当前稳定版建立 nightly matrix；
- DTLS 证书轮换和 fingerprint 双证书过渡；
- STUN、RTP、RTCP、SCTP、房间 Envelope 持续 fuzz；
- 定义协议兼容版本和弃用策略。

退出指标：连续 7 天互通 CI 通过，fuzz 累计 10 亿输入无 crash。

### Sprint 4-7：实时数据面认证

- SFU 多发布/多订阅和 Simulcast/SVC 压测；
- NACK、PLI、Transport-CC、RR 在 0-20% 丢包下校准；
- 音频优先、层切换滞回和关键帧请求节流；
- SCTP/DCEP 重传、stream reset、异常关联回收；
- TURN UDP/TCP/TLS、NAT 类型和 relay 端口耗尽测试。

退出指标：50 人房间、720p 三层、音频 p99 不因视频拥塞中断。

### Sprint 8-10：控制面与数据一致性

- PostgreSQL schema、事务 repository 和迁移工具；
- Redis/NATS 房间事件和幂等缓存；
- API/gateway/worker 任务所有权与 leader election；
- token issuer 接企业身份系统，支持 key rotation/revocation；
- 礼物对账、退款和审计 outbox。

退出指标：控制面可横向扩展，节点重启不丢业务状态，重复命令结果一致。

### Sprint 11-13：实时 MVP

- P2P 完整协商、TURN fallback 和 ICE restart；
- Web SDK 重连、token refresh、DataChannel/WSS fallback；
- media-node placement、draining 和 admission control；
- Prometheus SLI、首版容量模型和 on-call runbook；
- 24 小时实时 soak。

退出指标：实时 MVP 发布候选，核心接口兼容冻结。

### Sprint 14-16：直播

- WebRTC/WHIP ingest 到多 rendition CMAF/HLS；
- segment 原子发布、discontinuity、重启恢复；
- 对象存储和 CDN origin adapter；
- 直播录制、回放生成和保留策略；
- 首帧、卡顿率、转码延迟 SLI。

退出指标：6 小时连续直播无 manifest 断裂，worker 故障自动恢复。

### Sprint 17-19：点播与 SDK

- 大文件分片上传、校验、秒传和取消；
- probe、转码模板、缩略图、Range/HLS；
- Rust/C ABI 稳定性和符号版本；
- Kotlin/Swift 原生 WebRTC adapter 与后台/前台恢复；
- 五 SDK conformance suite 和示例应用。

退出指标：SDK 行为一致，常见 MP4/WebM 输入形成可播放资产。

### Sprint 20-23：RC

- 多 media-node 调度、房间亲和和容量回压；
- 节点故障重建、滚动升级和连接排空；
- 多可用区数据复制和灾难恢复演练；
- 第三方安全审计、依赖 SBOM、镜像签名；
- 1/10/50/200/1000 人容量曲线和成本模型。

退出指标：RTO/RPO 达标，无阻断级安全问题，发布/回滚自动化。

### Sprint 24-26：Production v1

- 48 小时混合媒体 soak；
- 网络分区、磁盘满、证书过期、worker crash 演练；
- 1% → 10% → 50% → 100% 灰度；
- 告警有效性、值班手册和客户支持交接；
- 最终兼容矩阵、API/SDK 文档和容量建议。

退出指标：所有发布门禁通过，SLO、责任人、回滚点和数据恢复流程签字确认。

## 4. 质量门禁

每个 PR：

- `cargo fmt --all -- --check`；
- `cargo clippy --workspace --all-targets -- -D warnings`；
- `cargo test --workspace --locked`；
- OpenSSL DTLS 特性单独编译和测试；
- Web SDK TypeScript check/build；
- 协议输入变更必须增加测试向量或 fuzz corpus。

每个 Release Candidate：

- Chrome/Firefox/Safari 和 Android/iOS 兼容矩阵；
- TURN UDP/TCP/TLS 与企业防火墙场景；
- 2%、5%、10%、20% 丢包和 100-1500 ms RTT；
- CPU、内存、带宽、relay 端口和 worker 并发容量；
- 漏洞扫描、密钥轮换、备份恢复和回滚演练。

## 5. 主要风险

| 风险 | 早期信号 | 处理 |
|---|---|---|
| 浏览器协议差异 | nightly interop 波动 | 固定 corpus，三浏览器门禁，版本灰度 |
| 自研 SCTP/SRTP 缺陷 | fuzz crash、重传耗尽 | 有界状态、差分测试、安全审计 |
| 转码资源失控 | worker queue 和 CPU 持续增长 | quota、admission、独立池、HLS fallback |
| TURN 端口耗尽 | allocation 超过端口池 80% | 固定端口池、告警、扩容或分片 |
| 文件状态阻碍扩容 | 多副本出现冲突 | 在 Sprint 8-10 完成事务存储迁移 |
| 礼物资金风险 | 重复 receipt 或账不平 | 可信验证、幂等 ledger、outbox、对账 |
| 周期被低估 | 互通/soak 未通过仍推进 | 退出条件优先，不以删减安全门禁换日期 |
