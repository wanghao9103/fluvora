# Fluvora API 与 SDK 接口

[简体中文](API.md) | [English](en/API.md)

本文描述 Production Candidate v1 的公共控制接口。默认入口为 `http://127.0.0.1:8080`，所有
`/v1` 请求均使用：

```http
Authorization: Bearer <token>
Content-Type: application/json
```

会改变状态的业务请求还应带不超过 128 字节的 `Idempotency-Key`。SDK 会自动生成。
错误统一返回：

```json
{"code":"machine_readable_code","message":"bounded explanation"}
```

标识符采用无前缀十六进制；token 由 `fluvora-admin token` 签发。可用 scope 为
`room_create`、`room_join`、`media_publish`、`room_moderate`、`gift_verify`、
`vod_manage`、`live_manage`、`token_revoke` 和内部的 `node_status_write`。

## 载荷和回放边界

所有普通 JSON 请求体最大 1 MiB；VOD 上传块、直播初始化段和直播媒体分片最大 8 MiB，
WHIP/WHEP 原始请求使用各自更小的上限。
四端 SDK 会在发起网络请求前执行同一组校验：聊天正文 1–4096 UTF-8 字节，自定义 namespace
为 1–64 个安全 ASCII 字符，自定义 JSON payload 最大 60 KiB，P2P signal payload 最大 64 KiB，
SDP 最大 256 KiB，媒体上传在 1–8 MiB 之间。单次信令拉取、WebSocket 初始回放和实时广播队列均最多 128 条；每房间
回放缓存最多保留 128 条且不超过 8 MiB，以较早达到的边界为准。

## 房间与互动

| 方法 | 路径 | 用途 |
|---|---|---|
| POST | `/v1/rooms` | 创建 `sfu`、`p2p`、`live` 或 `vod` 房间 |
| GET | `/v1/rooms/{room_id}` | 读取房间快照 |
| POST | `/v1/rooms/{room_id}/join` | 加入房间 |
| POST | `/v1/rooms/{room_id}/leave` | 离开并回收该参与者的媒体资源 |
| POST | `/v1/rooms/{room_id}/end` | 结束房间并回收所有会话、轨道和任务 |
| POST | `/v1/rooms/{room_id}/roles` | 设置参与者角色 |
| POST | `/v1/rooms/{room_id}/chat` | 写入持久聊天事件 |
| POST | `/v1/rooms/{room_id}/custom` | 写入带类型名的扩展事件 |
| POST | `/v1/rooms/{room_id}/gifts` | 记录可信支付端验证后的礼物凭据 |
| POST | `/v1/rooms/{room_id}/events/tickets` | 签发一次性 WebSocket ticket |
| GET | `/v1/rooms/{room_id}/events?ticket=...` | 订阅有序房间事件 |

创建房间示例：

```json
{"mode":"sfu","max_members":50,"max_publishers":10}
```

聊天和自定义事件分别使用：

```json
{"message_id":"client-unique-id","text":"hello"}
{"namespace":"com.example.whiteboard","schema_version":1,"payload":{"x":12,"y":8}}
```

礼物接口只允许持有 `gift_verify` 的可信服务调用，并以支付方 `transaction_id` 幂等，不应从
不可信客户端直接调用。请求包含 `provider`、`provider_timestamp_millis`、
`provider_signature`、`sender_id`、`recipient_id`、`transaction_id`、`gift_id`、
`quantity`、`unit_value` 和 `currency`。签名为 `FLUVORA_GIFT_WEBHOOK_SECRET` 上的
HMAC-SHA256（base64url 无 padding），覆盖 v1 domain separator、长度前缀字符串字段和
大端整数；时间戳允许窗口为 ±5 分钟。签名必须是 32 字节，交易号和礼物号分别限制为
512 和 256 个 UTF-8 字节，用户 ID 必须是 32 位十六进制，币种必须是 3 位大写 ASCII。

## WebRTC 与 SFU

| 方法 | 路径 | 用途 |
|---|---|---|
| GET | `/v1/rooms/{room_id}/ice-servers` | 获取短期 TURN REST 凭据 |
| POST | `/v1/rooms/{room_id}/webrtc/offer` | 标准 SDK offer/answer |
| POST | `/v1/rooms/{room_id}/whip` | 创建 WHIP 发布会话 |
| PATCH/DELETE | `/v1/rooms/{room_id}/whip/{session_id}` | Trickle ICE、ICE restart 或销毁 |
| POST | `/v1/rooms/{room_id}/whep` | 创建 WHEP 播放会话 |
| PATCH/DELETE | `/v1/rooms/{room_id}/whep/{session_id}` | Trickle ICE、ICE restart 或销毁 |

WHIP/WHEP `PATCH` 必须携带当前资源的强 `If-Match` ETag。Trickle ICE 片段中
`ice-ufrag` 和 `ice-pwd` 不得重复，并必须同时保持当前代际或同时切换到新代际；
凭据长度、单行长度、候选与 `mid` 非空性均在进入媒体节点前校验。
| POST | `/v1/rooms/{room_id}/tracks` | 注册发布轨道和 simulcast encoding |
| DELETE | `/v1/rooms/{room_id}/tracks/{track_id}` | 停止发布并清理订阅 |
| POST | `/v1/rooms/{room_id}/subscriptions` | 建立 SFU 下行并协商媒体路径 |
| DELETE | `/v1/rooms/{room_id}/subscriptions/{subscription_id}` | 取消订阅 |
| POST | `/v1/rooms/{room_id}/subscriptions/{subscription_id}/layer` | 手动设置空间/时间层 |

SDK 协商顺序为：

1. 创建标准 `RTCPeerConnection`；
2. 创建可靠有序的 `fluvora.room.v1` DataChannel；
3. 添加 transceiver，生成并设置本地 Offer；
4. POST Offer，设置服务端 Answer；
5. 注册发布轨道/订阅，或让 P2P 房间走下节的端到端信令。

订阅请求可携带 `subscriber_codecs`、`network_quality`、目标分辨率、帧率和码率。
服务端依次选择直接 SFU、共享实时转码或 `hls_fallback_url`，响应的 `path` 为
`direct`、`transcode`、`hls` 或幂等命中的 `existing`。Transport-CC 会在运行期间继续
自动升降 simulcast/SVC 层。

WHIP/WHEP 使用 `application/sdp`；PATCH 接受
`application/trickle-ice-sdpfrag`。带新 ICE ufrag/pwd 的 fragment 会在同一资源上执行
ICE restart，响应通过 `ETag` 和 `Location` 维护资源版本。

## P2P 信令

| 方法 | 路径 | 用途 |
|---|---|---|
| POST | `/v1/rooms/{room_id}/signals` | 写入 offer/answer/ice-candidate/ice-restart/renegotiate/bye |
| GET | `/v1/rooms/{room_id}/signals?after={sequence}` | 拉取增量信令 |

信令体：

```json
{
  "to": "optional-peer-hex-id",
  "kind": "offer",
  "payload": {"sdp": "..."}
}
```

媒体默认端到端直连；失败时客户端使用 `/ice-servers` 返回的 TURN UDP、TCP 或 TLS
候选。服务端只保存有界、递增序号的信令 backlog，不接触 P2P 媒体。

## DataChannel 房间数据

`fluvora.room.v1` 使用二进制 Envelope v1。客户端可发送 `chat`、`control`、`custom`；
服务端验证 token 绑定的参与者，重写 room/sender/sequence/timestamp 后再广播。
`gift` 和 `presence` 只能由可信控制面生成。单条完整消息上限 16 KiB，其中固定头部 60
字节，应用 payload 上限 16,324 字节；浏览器在读取 `Blob` 前检查大小，服务端收发使用相同
上限。

其他 DataChannel label 可在同一房间内中转字符串或二进制扩展数据。SCTP 数据面支持可靠
有序/无序通道，以及 DCEP `maxRetransmits`、`maxPacketLifeTime` 部分可靠通道。部分可靠
能力通过 INIT/INIT-ACK 协商；达到限次或限时条件后按消息放弃并发送 FORWARD-TSN，不阻塞
后续消息。FORWARD-TSN 会持续按超时重发，直到对端累计 SACK 已推进；对端没有协商
PR-SCTP 时明确拒绝部分可靠 OPEN。

## 点播

| 方法 | 路径 | 用途 |
|---|---|---|
| POST | `/v1/assets` | 创建资产 |
| GET | `/v1/assets/{asset_id}` | 查询上传/处理状态和播放 URL |
| DELETE | `/v1/assets/{asset_id}` | 幂等删除资产及其对象 |
| PATCH | `/v1/assets/{asset_id}/source?offset={n}` | 按偏移续传二进制 source |
| POST | `/v1/assets/{asset_id}/complete` | 固化源文件并提交 probe/transcode |

完成请求包含 `source_bytes`、`segment_duration_millis` 和 ABR `renditions`。worker 使用
ffprobe 验证输入，通过 FFmpeg 生成原子发布的 HLS；状态按
`created → uploading → uploaded → probing → transcoding → ready` 推进，失败时提供有界
原因和 `retryable`。

媒体网关（默认 8093）提供 manifest、segment 和源文件 Range 读取。网关公开地址由
`FLUVORA_PUBLIC_MEDIA_BASE_URL` 决定。

API 到媒体网关的控制代理保留成功状态和业务 4xx；上游重定向、5xx、超限响应或非 JSON
控制响应统一返回 502。媒体上传体以共享字节缓冲区转交内部 HTTP 客户端，避免分片整块复制；
空响应不伪造 `Content-Type`，非空控制响应固定为经过校验的 JSON。

## 直播

| 方法 | 路径 | 用途 |
|---|---|---|
| POST/GET | `/v1/live/{stream_id}` | 创建或查询直播 HLS 输出 |
| DELETE | `/v1/live/{stream_id}` | 停止并删除直播输出 |
| PUT | `/v1/live/{stream_id}/init` | 上传 CMAF init segment |
| PUT | `/v1/live/{stream_id}/segments/{sequence}` | 顺序上传 media segment |
| POST | `/v1/live/{stream_id}/finish` | 写入 ENDLIST 并结束 |

创建时可直接给出 `source_tracks`，media-node 会把指定 SFU RTP 轨道送入实时 worker，
完成 WebRTC/WHIP 到 HLS 的管线。可选的 `renditions` 使用与点播相同的
`width/height/video_bitrate/audio_bitrate` 结构，最多 8 档；配置后返回 `master.m3u8`，
每档使用独立的 rendition playlist、init segment 和 media segment。未配置时保持单档
`index.m3u8` 兼容行为。也可由外部打包器上传 CMAF 片段。所有 manifest 只引用已经原子
落盘的 segment，并维护有界直播窗口。

## SDK

五端的安装、初始化、SFU/P2P 完整流程、原生 WebRTC 注入、错误重试、token 刷新和
资源释放见 [《SDK 接入指南》](SDK_INTEGRATION.md)。

| SDK | 目录 | WebRTC 接入方式 |
|---|---|---|
| Web/TypeScript | `sdk/web` | 原生 `RTCPeerConnection`，内置 DataChannel 和弱网路径 |
| Rust | `sdk/rust` | 实现异步 `WebRtcPeer` trait |
| C ABI | `sdk/c-abi` | 稳定 FFI 基础控制接口，返回 JSON 供引擎绑定 |
| Android/Kotlin | `sdk/android` | 实现 `WebRtcPeer`，接应用选择的原生 WebRTC |
| iOS/Swift | `sdk/ios` | 实现 `WebRTCPeer`，接应用选择的原生 WebRTC |

Web、Rust、Android 与 iOS SDK 统一拒绝携带用户信息、查询或片段的基础 URL，以及空值、超过
4096 字节或含控制字符的访问令牌；控制请求不跟随重定向。成功 JSON 响应最多 32 MiB，错误
响应最多 64 KiB，并在流式读取时执行上限，避免服务端省略或伪造 `Content-Length` 后造成无界
内存增长。Web 的异步令牌刷新结果会在每次请求前重新校验；iOS 对调用方注入会话的最终 URL
也会复核。基础 URL 中的路径前缀会由四端一致保留，支持挂载在反向代理子路径下。

原生适配器的 `prepareRoomDataChannel` 在创建 Offer 前调用，媒体-only 适配器可以保留
默认空实现。Fluvora 不强制捆绑某个客户端 WebRTC 二进制，因此能与浏览器以及符合标准的
原生实现协作。

## 运维接口

每个服务均提供 `/health/live`、`/health/ready` 和 `/metrics`。状态服务还提供
`/v1/status` 聚合五类节点心跳和容量。Prometheus 只抓取内网 metrics；业务 token、内部
服务 token、TURN secret 和证书都不得放入指标或日志。
