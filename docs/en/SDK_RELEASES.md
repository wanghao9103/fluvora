# Independent SDK Versioning and Releases

[简体中文](../SDK_RELEASES.md) | [English](SDK_RELEASES.md)

## Decision

Fluvora keeps a single source repository so protocol, server, and SDK changes can be reviewed atomically. Delivery boundaries are independent: the platform and all five SDKs own separate versions, changelogs, artifacts, and release tags. An SDK-only fix neither bumps nor builds the server or container image.

| Release unit | Version source | Changelog | Tag |
| --- | --- | --- | --- |
| Server/platform | root `Cargo.toml` | `/CHANGELOG.md` | `vX.Y.Z` |
| Web SDK | `sdk/web/package.json` | `sdk/web/CHANGELOG.md` | `web-vX.Y.Z` |
| Rust SDK | `sdk/rust/Cargo.toml` | `sdk/rust/CHANGELOG.md` | `rust-vX.Y.Z` |
| C ABI SDK | `sdk/c-abi/Cargo.toml` | `sdk/c-abi/CHANGELOG.md` | `c-abi-vX.Y.Z` |
| Android SDK | `sdk/android/build.gradle.kts` | `sdk/android/CHANGELOG.md` | `android-vX.Y.Z` |
| Swift SDK | `sdk/ios/VERSION` | `sdk/ios/CHANGELOG.md` | `swift-vX.Y.Z` |

## Local builds

```powershell
./scripts/build-sdk.ps1 -Sdk web
./scripts/build-sdk.ps1 -Sdk rust
./scripts/build-sdk.ps1 -Sdk c-abi
./scripts/build-sdk.ps1 -Sdk android
./scripts/build-sdk.ps1 -Sdk swift
```

Artifacts are written under `artifacts/sdk-releases/<sdk>/` with the package/binaries, matching SDK source and demo, bilingual integration guides, a manifest, and SHA-256 checksums. A clean worktree is required by default; `-Version` verifies that a pipeline tag matches the source version.

## Changelog policy

- The root changelog does not duplicate SDK-only fixes; it links an SDK only for a platform behavior or compatibility milestone.
- An SDK changelog covers only that SDK's public API, behavior, fixes, and migration notes, including its compatible server range and protocol identifier.
- One commit may change several release units, while each unit independently decides whether to bump and tag.
- The current workflow builds candidates only; it does not publish npm, Maven, crates.io, or GitHub Releases.
