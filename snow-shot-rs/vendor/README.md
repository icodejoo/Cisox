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

## 本地补丁登记

升级 gpui 时必须带上或确认上游已修复下列补丁；补丁待评审（见 `git diff -- vendor/`）。

| 补丁 | 位置 | 原因 | 效果（Intel UHD 770，双屏 2560x1440 整屏覆盖窗） |
|---|---|---|---|
| 渲染器销毁后收尾 GPU 状态 | `gpui-pre-windows` `directx_renderer.rs` 的 `GpuReleaseGuard`（`DirectXRenderer` 最后一个字段） | 共享立即上下文关窗后仍绑着旧资源，驱动池也不归还，关覆盖窗后私有提交长期偏高 | 截图一次后私有提交 169MB → 135MB，私有工作集 137MB → 103MB，释放从约 10s 缩到 0.5s；收尾用显式解绑（`unbind_window_resources`：渲染目标 + VS/PS 资源槽 0..2 + `Flush` + `Trim`），**不用 `ClearState`**：Intel 驱动 31.0.101.4953 每次 `ClearState`（画过东西后）会在 UMD 里漏一个永不关闭的内核 Event，在简单探针里确实每轮 +1（见 `docs/design/tray-host-split.md` §12.10）；但产品里每次关窗约 9 个的主因不是它，而是每窗口新建着色器，见下一行。单测 `release_guard_tests`（WARP 软件设备）：vendor 不是 workspace 成员，需把该 crate 拷到 workspace 外、追加空 `[workspace]` 后 `cargo test --offline --lib --features test-support release_guard` |
| 着色器 / 混合状态设备级共享 | `gpui-pre-windows` `directx_renderer.rs` 的 `PipelineObjectCache`（挂在 `directx_devices.rs` 的 `DirectXDevices.pipeline_cache`，`PipelineState::new` 经它取对象） | 原实现每窗口各建 8 条管线的 VS / PS / 混合状态；Intel UHD 770 驱动（31.0.101.4953）在这些新着色器首次绘制时，于驱动内部工作线程（`igd10um64xe`，线程入口 `OpenAdapter10_2+0x44aff`）里留下永不关闭的内核 Event，关一次窗口漏约 9 个句柄（独立探针 `dxprobe2` 复现：每轮 +9；只共享着色器 + 混合状态后预热 3 轮起每轮 0，实例缓冲仍每窗口自建）。着色器 / 混合状态是不可变对象，设备内建一次即可，同时省掉每窗口的建着色器耗时 | 设备丢失重建时随新 `DirectXDevices` 换新缓存，不会混用旧设备对象。单测 `pipeline_cache_tests`（WARP，同 `release_guard` 的跑法）验证同缓存两套管线共用着色器 / 混合状态、实例缓冲独立、不同缓存互不串用。**离屏的真渲染器小场景（Quad / Shadow / Underline / 图片精灵）复现不出泄漏，无法离屏回归；句柄是否不再增长要靠真屏连截复测** |
