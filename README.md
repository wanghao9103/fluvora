# Fluvora（澜曜）

[简体中文](README.md) | [English](README.en.md)

Fluvora 是一个 Rust 流媒体平台参考实现。实时媒体核心从零实现，不接入现成 WebRTC/SFU
服务器内核；通用密码原语交给 OpenSSL/RustCrypto，音视频编解码与封装交给 FFmpeg。

已覆盖：

- WebRTC ICE-lite、STUN、SDP、DTLS-SRTP、RTP/RTCP；
- P2P 信令和 TURN UDP/TCP/TLS 中继；
- 单节点 SFU、Simulcast/SVC 层选择、NACK、PLI、Transport-CC；
- WebRTC DataChannel（自研 SCTP、DCEP、CRC32C、SACK、PR-SCTP、FORWARD-TSN、分片和 stream reset）；
- WHIP/WHEP、Trickle ICE 和会话内 ICE restart；
- 实时转码、故障自动重建、WebRTC 到 HLS；
- 直播/点播多码率 CMAF/HLS、录制、上传、探测、转码和播放；
- 聊天、已验证礼物、P2P 信令和自定义房间数据；
- Web、Rust、C ABI、Android/Kotlin、iOS/Swift SDK；
- 服务心跳、容量感知节点调度、优雅排空、Prometheus、Grafana、Compose、Kubernetes 和 CI。

## 快速启动

需要 Docker Compose。复制并修改开发环境配置：

```powershell
Copy-Item deploy/compose/.env.example deploy/compose/.env
docker compose --env-file deploy/compose/.env -f deploy/compose/compose.yaml up --build -d
```

本地入口：

- API：`http://127.0.0.1:8080`
- 媒体文件/HLS：`http://127.0.0.1:8093`
- 平台状态：`http://127.0.0.1:8090/v1/status`
- Prometheus：`http://127.0.0.1:9090`
- Grafana：`http://127.0.0.1:3000`
- Alertmanager：`http://127.0.0.1:9093`
- TURN：UDP/TCP `3478`、TLS `5349`、relay UDP `49152-49251`

签发一小时的开发令牌：

```powershell
docker compose --env-file deploy/compose/.env -f deploy/compose/compose.yaml run --rm api `
  fluvora-admin token --subject 1 --room * --ttl 3600 --scopes all
```

生产环境必须替换所有示例密钥、域名、证书和公网 IP。TURN relay 端口范围必须同时在防火墙、
容器编排和 `FLUVORA_TURN_RELAY_PORT_MIN/MAX` 中开放。

## 本地验证

统一门禁会输出机器可读的 `artifacts/release-gates-*/release-gates.json` 和逐项日志：

```powershell
./scripts/run-release-gates.ps1 -Profile quick
./scripts/run-release-gates.ps1 -Profile full
```

`quick` 包含 Rust 全工作区、SDK 契约、Web SDK 和真实 TURN UDP/TCP/TLS 中继；`full`
另外包含生产 DTLS、实时转码、直播/点播 HLS、容量及浏览器/控制面短时 soak。也可分别运行：

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --locked
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/check-docs.ps1
cargo run --release --locked -p fluvora-perf-lab -- --quick --assert
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/smoke-hls-pipelines.ps1
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/smoke-turn.ps1

cd sdk/web
npm ci
npm run check
npm run build
```

从干净提交构建带版本号的候选交付包：

```powershell
./scripts/build-release.ps1 -CiRunUrl "https://github.com/<owner>/<repo>/actions/runs/<id>"
```

脚本校验 Cargo、Web 与 Android 版本一致性，执行快速发布门禁，并输出服务端二进制、五端 SDK
产物/示例、双语文档、源码快照、构建证据和 SHA-256 校验和到 `artifacts/releases/`。候选包不会
自动创建 Git tag、GitHub Release，也不会发布到 npm、Maven 或其他公共仓库。

五端 SDK 接入示例见 [`examples/README.md`](examples/README.md)，覆盖建房/入房、SFU、
P2P 信令、ICE、聊天、自定义数据和资源清理。详细能力矩阵及原生 WebRTC 引擎边界见
[`docs/SDK_DEMOS.md`](docs/SDK_DEMOS.md)。

真实浏览器、P2P/SFU、弱网和控制面负载由 CI/Release 的
`scripts/run-browser-interop.sh` 执行。控制面容量或 soak 可直接运行：

Windows 开发机可用 `scripts/run-browser-interop.ps1` 启动真实 Rust API/media-node，并运行
Chromium SFU、可靠/部分可靠 DataChannel、媒体转发和 P2P 测试。

```powershell
$env:FLUVORA_LOAD_TOKEN = "<short-lived token>"
node scripts/load-control-plane.mjs --profile capacity
$env:FLUVORA_LOAD_TOKEN = $null
node scripts/load-control-plane.mjs --profile soak --token-file C:\secure\fluvora-soak.token
```

长稳压测期间由受控签发器原子替换 token 文件，压测器每 30 秒热加载，并在 401 时立即重载。

公网 TURN 验收可在另一公网节点启动 UDP echo，再从客户端网络分别运行 UDP/TCP/TLS 探针：

```bash
fluvora-turn-probe echo --bind 0.0.0.0:3479
fluvora-turn-probe probe --transport tls --server turn.example.com:5349 \
  --server-name turn.example.com --username "$TURN_USERNAME" \
  --password-file /run/secrets/turn-password --peer echo.example.net:3479 \
  --evidence artifacts/turn-tls.json
```

也可由密钥注入器提供 `FLUVORA_TURN_PROBE_PASSWORD`，避免密码出现在命令行和进程列表。

完整 DTLS 构建在 Linux 上使用：

```bash
sudo apt-get install libssl-dev pkg-config ffmpeg
cargo clippy -p fluvora-media-node --features openssl-backend --all-targets -- -D warnings
```

Windows 若没有系统 OpenSSL，可启用 `openssl-vendored`；该模式还需要完整 Perl 和 MSVC
构建工具。含中文的源码路径应使用脚本提供的 ASCII Cargo 缓存：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/check-openssl-vendored.ps1
```

CI 与容器镜像始终编译生产 DTLS 特性。

## 目录

- `crates/foundation/`：领域模型、公共协议、字节编解码和可观测性；
- `crates/webrtc/`：STUN、ICE、DTLS、SRTP、RTP/RTCP、DataChannel 和 SFU；
- `crates/media/`：媒体存储、FFmpeg/HLS 管线和转码决策；
- `crates/control-plane/`：鉴权、持久化、事件和服务状态；
- `crates/services/`：API、媒体节点、worker、gateway 和 TURN 可部署进程；
- `crates/tools/`：管理 CLI 和性能门禁工具；
- `sdk/`：Web、Rust、C、Android、iOS SDK；
- `deploy/`：容器、Compose、Kubernetes、监控和告警；
- `fuzz/`：STUN、RTP、SCTP/DataChannel 模糊测试入口；
- `scripts/`：统一发布门禁、真实 TURN、FFmpeg 实时转码、点播多码率 fMP4/HLS 和直播 RTP→HLS 冒烟测试；
- `tests/browser/`：真实浏览器 DataChannel、VP8 经 SFU 转发、P2P 互通探针与 Playwright 矩阵；
- `docs/`：架构、安全边界、验收门禁、运维手册和开发周期。

完整阅读路径见 [docs/README.md](docs/README.md)。第一次进入仓库建议先阅读
[docs/CODEBASE.md](docs/CODEBASE.md)，总体设计见
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)，API 内部分层见
[docs/API_SERVER_STRUCTURE.md](docs/API_SERVER_STRUCTURE.md)，公开接口见
[docs/API.md](docs/API.md)，SDK 接入见
[docs/SDK_INTEGRATION.md](docs/SDK_INTEGRATION.md)，发布门禁见
[docs/PRODUCTION_ACCEPTANCE.md](docs/PRODUCTION_ACCEPTANCE.md)，运维处置见
[docs/RUNBOOK.md](docs/RUNBOOK.md)。

## SDK 独立版本与发布

源码保持在同一仓库，便于协议、服务端和 SDK 的关联修改原子评审；版本和交付则按 SDK
拆分。运行 `./scripts/build-sdk.ps1 -Sdk web` 只构建 Web SDK，推送 `web-vX.Y.Z` 也只触发
Web SDK 候选包，不构建服务端或容器。Rust、C ABI、Android、Swift 分别使用 `rust-v*`、
`c-abi-v*`、`android-v*`、`swift-v*`。

根目录 `CHANGELOG.md` 只记录服务端/平台，各 SDK 在自己的 `sdk/<name>/CHANGELOG.md`
记录修复、迁移和服务端兼容范围，避免 SDK 小修复淹没平台记录。完整约定见
[SDK 独立版本与发布设计](docs/SDK_RELEASES.md)。
