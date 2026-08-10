# SDK 示例交付规范

[简体中文](SDK_DEMOS.md) | [English](en/SDK_DEMOS.md)

实际业务接入步骤、平台依赖、WebRTC 适配器契约、错误重试和资源释放见
[《SDK 接入指南》](SDK_INTEGRATION.md)。本文只定义可运行示例的交付和验收范围。

## 目标

公开发布的 SDK 不能只提供库和片段代码。每个 Client 都必须有可构建的接入示例，并通过
`sdk-demo-contract-v1.json` 的共同场景检查。

## 五端能力矩阵

| 场景 | Web | Rust | C/C++ | Android | Swift/iOS |
| --- | --- | --- | --- | --- | --- |
| 短期 Token 与 API 配置 | 可运行 | 可运行 CLI | 可运行 CLI | 可运行 App | SwiftUI App + CLI |
| 创建/加入/离开房间 | 是 | 是 | 是 | 是 | 是 |
| 聊天与自定义数据 | REST + DataChannel | REST | REST | REST | REST |
| 房间 ICE/TURN 凭证 | 是 | 是 | 是 | 是 | 是 |
| SFU Offer/Answer | 浏览器真实媒体 | 宿主 Peer 回调 | 宿主 SDP | 宿主 Peer 回调 | 宿主 Peer 回调 |
| P2P Offer/Answer/ICE | 浏览器真实信令 | 信令 CLI | 信令 CLI | 信令调用 | 信令 CLI |
| 弱网统计和自适应 | 完整演示 | 由宿主 Peer 上报/执行 | 由宿主引擎执行 | 由宿主引擎执行 | 由宿主引擎执行 |
| Live/VOD Manifest | 视频控件 | URL/API | URL/API | 宿主播放器 | AVPlayer/宿主播放器 |
| 清理资源 | Tracks + Peer | 房间状态 | Client/字符串 | Engine + 房间 | Peer + 房间 |

这里的“宿主 Peer 回调”不是模拟 WebRTC。原生应用通常已经选择了适合自身 ABI、硬件编解码和
发布渠道的 WebRTC 实现；Fluvora SDK 负责 ICE 凭证、Offer/Answer 交换、房间信令和协议约束，
宿主实现负责媒体采集、`PeerConnection`、渲染和设备资源释放。浏览器端直接使用标准
`RTCPeerConnection`，因此可以端到端运行。

## 验收门禁

每次 CI 必须执行：

1. `node scripts/check-sdk-contract.mjs`：服务端路由与五 SDK API 对齐；
2. `node scripts/check-sdk-demos.mjs`：五端示例覆盖共同场景；
3. Rust example 编译；
4. C example 与 `fluvora-c-abi` 实际链接；
5. Android `:demo:assembleDebug`；
6. Swift demo product 编译；
7. 浏览器三引擎 WebRTC 互操作测试。

## 交付周期

当前代码阶段的五端示例和 CI 门禁已经实现。正式 GA 前的外部认证工作单独排期：

| 工作 | 周期 | 前置条件 | 产物 |
| --- | ---: | --- | --- |
| Android 主流 libwebrtc 发行版实机适配 | 3–5 天 | 选定 AAR/ABI 与设备矩阵 | Adapter、实机日志、APK |
| iOS WebRTC Framework 实机适配 | 3–5 天 | 选定 XCFramework 与签名团队 | Adapter、真机日志、IPA |
| Unity/Unreal C ABI 插件样例 | 5–8 天 | 明确引擎版本与目标平台 | 插件工程和编辑器冒烟 |
| 公网 NAT/TURN 互通认证 | 2–3 天 | 公网域名、证书、至少两类 NAT | 证据包 |
| Linux Chrome/Firefox/WebKit 弱网矩阵 | 2–3 天 | Linux runner/netem 权限 | 报告和截图 |
| 48 小时多节点稳定性与容灾演练 | 3–4 天 | 至少三节点和观测后端 | SLO/恢复报告 |
| 第三方安全与兼容审计 | 1–3 周 | 外部供应商 | 审计报告 |

这些任务依赖发布环境、签名资产、客户端二进制选择或外部团队，不能由仓库内单元测试替代。
