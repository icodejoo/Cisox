# vendor/：GPUI 依赖族锁定（ADR-1）

## 这里是什么

`gpui-pre` 0.3.7（Zed 快照 `zed@1a28cff`，由 huacnlee 以 `gpui-pre-*` 名义拆包发布）与
`gpui-kit` 0.7.0（longbridge/gpui-kit `0c830f4d`）及其同族 crate，共 28 个，约 27MB。
内容是 crates.io 发布包的**原样解压**（仅去掉 `.cargo-ok` 与被 `.gitignore` 忽略的 `Cargo.lock`），
根 `Cargo.toml` 的 `[patch.crates-io]` 逐个指向本目录。其余传递依赖（含 `zed-font-kit`、
`zed-scap`、`zed-xim`）仍走 crates.io，由 `Cargo.lock` 校验和锁定。

为什么不是 git submodule：`gpui-pre-*` 是重命名并改写过依赖的拆分包，Zed 仓库里没有同名
crate，无法直接 `[patch]` 到 Zed 检出目录；且 Zed 仓库约 517MB。详见迁移方案 ADR-1 修订建议。

## 约束

- 只有 `snow-ui-shell` 的 `Cargo.toml` 可以声明 gpui / gpui-kit（`tools/workspace-guard` 强制）。
- 版本一律 `=` 精确锁定，禁止 `cargo update -p gpui-pre` 之类的隐式升级。
- 不要手改本目录源码；确需补丁，另开评审并在此登记。

## 升级流程

1. **选提交**：在 crates.io 查 `gpui-kit` 新版本，读其 `Cargo.toml` 对 `gpui-pre` 的 `=x.y.z` 约束；
   `gpui-pre` 的 description 里含对应 Zed 提交号，记录到评审单。
2. **更新**：用临时目录 `cargo add`/`cargo fetch` 取新版，把 28 个同族 crate 的新版本目录
   放进 `vendor/`（旧目录删除），同步修改根 `[patch.crates-io]` 与 `snow-ui-shell/Cargo.toml`
   的 `=` 版本；运行 `cargo metadata` 更新 `Cargo.lock`，核对新增/移除的 crate 列表。
3. **回归**：在隔离 target 下跑 `cargo check/clippy -D warnings/test --workspace`，
   再逐个运行 `spikes/` 下 p0-v1（覆盖窗）、p0-v4（托盘热键）、p0-v5（gpui-kit 设置窗）、
   p0-v6（IME 文本）并对照原结论；`snow-ui-shell::run_smoke_app` 需能启动并可关闭。
4. **评审**：`git diff --stat vendor/` 与关键目录 diff（`gpui-pre-windows`、`gpui-pre-platform`、
   `gpui-pre`）交给评审人；公开 API 破坏点只允许在 `snow-ui-shell` 内修复。通过后单独提交。
