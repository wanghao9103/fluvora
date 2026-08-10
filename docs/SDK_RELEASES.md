# SDK 独立版本与发布

[简体中文](SDK_RELEASES.md) | [English](en/SDK_RELEASES.md)

## 设计结论

Fluvora 保持单一源码仓库，以便协议、服务端和 SDK 的关联修改可以在一个提交中原子评审；交付边界则完全拆分。平台与五个 SDK 分别维护版本、变更记录、构建产物和发布标签。SDK 的缺陷修复不会推动服务端版本，也不会构建服务端或容器镜像。

| 发布单元 | 版本来源 | Changelog | 标签 |
| --- | --- | --- | --- |
| 服务端/平台 | 根 `Cargo.toml` | `/CHANGELOG.md` | `vX.Y.Z` |
| Web SDK | `sdk/web/package.json` | `sdk/web/CHANGELOG.md` | `web-vX.Y.Z` |
| Rust SDK | `sdk/rust/Cargo.toml` | `sdk/rust/CHANGELOG.md` | `rust-vX.Y.Z` |
| C ABI SDK | `sdk/c-abi/Cargo.toml` | `sdk/c-abi/CHANGELOG.md` | `c-abi-vX.Y.Z` |
| Android SDK | `sdk/android/build.gradle.kts` | `sdk/android/CHANGELOG.md` | `android-vX.Y.Z` |
| Swift SDK | `sdk/ios/VERSION` | `sdk/ios/CHANGELOG.md` | `swift-vX.Y.Z` |

## 本地构建

```powershell
./scripts/build-sdk.ps1 -Sdk web
./scripts/build-sdk.ps1 -Sdk rust
./scripts/build-sdk.ps1 -Sdk c-abi
./scripts/build-sdk.ps1 -Sdk android
./scripts/build-sdk.ps1 -Sdk swift
```

产物写入 `artifacts/sdk-releases/<sdk>/`，包含二进制/包、对应 SDK 源码和 Demo、双语接入文档、清单及 SHA-256 校验。默认要求干净工作区；`-Version` 可校验流水线标签与源码版本一致。

## 变更记录规则

- 根 Changelog 不重复抄写 SDK 内部修复；只有影响平台行为或兼容性的里程碑才记录关联 SDK。
- SDK Changelog 只记录该 SDK 的公开 API、行为、修复和迁移说明，并明确兼容的服务端范围与协议代号。
- 同一提交可以同时修改多个发布单元，但每个单元独立决定是否升版和打标签。
- 当前流水线只生成候选包，不自动发布 npm、Maven、crates.io 或 GitHub Release。
