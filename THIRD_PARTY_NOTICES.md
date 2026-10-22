# Third-Party Notices

本文件列出 PRTS Terminal 0.1.0 随包分发或运行时使用的第三方组件，以及各自的许可证声明。它不替代各组件自身的许可证全文；组件包内附带的许可证文本仍然有效。

## PRTS Terminal 核心

- 许可证：Apache License 2.0
- 全文：仓库根 `LICENSE`
- 版权：AirsonVind 及贡献者

## PRTS MAA Bridge

- 许可证：AGPL-3.0-or-later
- 全文：`bridge/LICENSE`
- 说明：bridge 是独立进程，运行时加载用户自行安装的 MaaCore.dll；本项目不分发 MAA 本体或 MaaCore 运行库。bridge 与核心只通过本机 IPC 通信。
- 源码：bridge 源码随本仓库提供；分发 bridge 二进制时，按 AGPL 要求提供对应源码或获取方式。

## Rust 运行时依赖（PRTS Terminal 核心）

下表由 `Cargo.lock` 与 `cargo metadata` 生成，仅列出运行时依赖；构建依赖与开发依赖不随安装包分发。

| Package | Version | License | Source |
|---|---|---|---|
| `chrono` | 0.4.44 | MIT OR Apache-2.0 | https://github.com/chronotope/chrono |
| `encoding_rs` | 0.8.35 | (Apache-2.0 OR MIT) AND BSD-3-Clause | https://github.com/hsivonen/encoding_rs |
| `futures-util` | 0.3.32 | MIT OR Apache-2.0 | https://github.com/rust-lang/futures-rs |
| `iana-time-zone` | 0.1.65 | MIT OR Apache-2.0 | https://github.com/strawlab/iana-time-zone |
| `image` | 0.25.10 | MIT OR Apache-2.0 | https://github.com/image-rs/image |
| `log` | 0.4.30 | MIT OR Apache-2.0 | https://github.com/rust-lang/log |
| `readabilityrs` | 0.1.4 | Apache-2.0 | https://github.com/theiskaa/readabilityrs |
| `regex` | 1.12.3 | MIT OR Apache-2.0 | https://github.com/rust-lang/regex |
| `reqwest` | 0.12.28 | MIT OR Apache-2.0 | https://github.com/seanmonstar/reqwest |
| `rusqlite` | 0.40.2 | MIT | https://github.com/rusqlite/rusqlite |
| `semver` | 1.0.28 | MIT OR Apache-2.0 | https://github.com/dtolnay/semver |
| `serde` | 1.0.228 | MIT OR Apache-2.0 | https://github.com/serde-rs/serde |
| `serde_json` | 1.0.150 | MIT OR Apache-2.0 | https://github.com/serde-rs/json |
| `tauri` | 2.11.2 | Apache-2.0 OR MIT | https://github.com/tauri-apps/tauri |
| `tauri-plugin-dialog` | 2.7.3 | Apache-2.0 OR MIT | https://github.com/tauri-apps/plugins-workspace |
| `tauri-plugin-opener` | 2.5.4 | Apache-2.0 OR MIT | https://github.com/tauri-apps/plugins-workspace |
| `tauri-plugin-prevent-default` | 5.0.2 | MIT | https://github.com/ferreira-tb/tauri-plugin-prevent-default |
| `tauri-plugin-single-instance` | 2.4.5 | Apache-2.0 OR MIT | https://github.com/tauri-apps/plugins-workspace |
| `tauri-plugin-updater` | 2.12.0 | Apache-2.0 OR MIT | https://github.com/tauri-apps/plugins-workspace |
| `tokio` | 1.52.3 | MIT | https://github.com/tokio-rs/tokio |
| `url` | 2.5.8 | MIT OR Apache-2.0 | https://github.com/servo/rust-url |
| `windows` | 0.61.3 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| `windows-capture` | 2.0.1 | MIT | https://github.com/NiiightmareXD/windows-capture |

## Rust 运行时依赖（PRTS MAA Bridge）

| Package | Version | License | Source |
|---|---|---|---|
| `libloading` | 0.8.9 | ISC | https://crates.io/crates/libloading |
| `serde_json` | 1.0.151 | MIT OR Apache-2.0 | https://crates.io/crates/serde_json |
| `windows` | 0.61.3 | MIT OR Apache-2.0 | https://crates.io/crates/windows |

## 前端运行时依赖

下表由 `package-lock.json` 生成，仅列出非开发依赖。

| Package | Version | License | Source |
|---|---|---|---|
| `@tauri-apps/api` | 2.11.0 | Apache-2.0 OR MIT | https://registry.npmjs.org/@tauri-apps/api/-/api-2.11.0.tgz |
| `@tauri-apps/plugin-opener` | 2.5.4 | MIT OR Apache-2.0 | https://registry.npmjs.org/@tauri-apps/plugin-opener/-/plugin-opener-2.5.4.tgz |
| `dompurify` | 3.4.14 | (MPL-2.0 OR Apache-2.0) | https://registry.npmjs.org/dompurify/-/dompurify-3.4.14.tgz |
| `marked` | 18.0.7 | MIT | https://registry.npmjs.org/marked/-/marked-18.0.7.tgz |
| `react` | 19.2.6 | MIT | https://registry.npmjs.org/react/-/react-19.2.6.tgz |
| `react-dom` | 19.2.6 | MIT | https://registry.npmjs.org/react-dom/-/react-dom-19.2.6.tgz |
| `svg-smoother` | 2.1.0 | GPL-3.0-or-later | https://registry.npmjs.org/svg-smoother/-/svg-smoother-2.1.0.tgz |
| `zustand` | 5.0.14 | MIT | https://registry.npmjs.org/zustand/-/zustand-5.0.14.tgz |

## 随包数据与内容

- **ArknightsGameData**：游戏数据表、关卡与资源数据的来源之一。上游仓库未声明许可证（license 为 null）；游戏原始数据及相关名称、内容的权利归鹰角网络所有。本项目按非官方、非商业同人项目使用，不主张对这些数据享有权利。
- **PRTS Wiki**：部分机制条目与文本参考 PRTS Wiki。Wiki 内容按 CC BY-NC-SA 4.0 使用；相关条目保留来源与署名信息，不用于商业用途，并以相同方式共享。
- **MAA / MaaCore**：不随本项目分发。用户需自行安装 MAA；bridge 仅在运行时加载用户环境中的 MaaCore.dll。
- **应用图标与界面素材**：除特别注明外，由本项目维护；不使用《明日方舟》官方 Logo 或官方美术作为应用标识。

## 说明

- 本文件中的“未声明”表示上游包没有在元数据中给出许可证字段，不代表该组件没有版权或可以无条件使用。
- 许可证文本以各组件包内文件为准；如本文件与组件自身许可证冲突，以组件自身许可证为准。
- 本文件不是法律意见；正式公开发布前应由维护者复核并补齐缺失的许可证文本。
