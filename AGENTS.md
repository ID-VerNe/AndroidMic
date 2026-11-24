# AGENTS.md

工作约定。改代码前先读这一页。

## 上游策略（硬性）

默认只在 fork `ID-VerNe/AndroidMic` 内工作。

**禁止**与 `upstream`（`teamclouday/AndroidMic`）发生任何交互，包括：

- 写：push、开 PR、评论 issue/PR、`gh pr ready`、close、rebase 到 upstream 分支
- 读：不主动 `git fetch upstream`、不 `gh pr list/view` 轮询上游状态、不向上游 issue 补评论

**唯一例外**：用户明确说「发 PR」（或"提交上游""给上游提"）时，才允许碰 upstream；动手前先确认要发哪些分支、基于哪个 base。

已经存在的上游 PR #193–#198 保持原样：不 rebase、不转 ready、不追进度。它们合不合、何时合，不作为本地工作的前提。

## 远端用途

- `origin` = fork，日常唯一读写目标：分支、issue、fork CI。
- `upstream` 只作为历史来源保留，日常不 fetch、不 merge。需要同步代码时先问。

## 工作约定

- issue 一律提到 fork：`gh issue create -R ID-VerNe/AndroidMic`。
- 验证走 fork CI，别在本地硬啃构建：
  ```bash
  gh workflow run CI --repo ID-VerNe/AndroidMic --ref <branch>
  gh run list --repo ID-VerNe/AndroidMic --limit 4
  ```
  CI 跑 `just build-debug`（Linux/Windows/macOS）+ `./gradlew assembleDebug`，**不跑单元测试**；本地 `cargo check` 不可用（`aws-lc-sys` 的 `cl.exe` 卡死，缺 protoc / libclang / rnnoise 模型）。
- 分支命名：`fix/…`、`feat/…`、`chore/…`。
- 探针与采样脚本放工作区 `probe/`，不进仓库。

## 仓库结构

- `RustApp/` — PC 端（Rust，cosmic/iced UI）；`Android/` — 手机端（Kotlin/Compose）。
- `RustApp/src/on_demand/` — 按需采集：`endpoint.rs`（端点选择）、`source_windows.rs`（WASAPI 会话枚举）、`state.rs`（demand 状态机）、`watcher.rs`（100 ms 轮询）。
- 配置：`%APPDATA%\teamclouday\AndroidMic\config\AndroidMic.toml`
- 日志：`%LOCALAPPDATA%\teamclouday\AndroidMic\cache\AndroidMic.log`（1 MB × 2 轮转，默认只记 warn 以上；要 INFO 需带 `RUST_LOG=info` 启动）
