# V6 证据说明

`ime-trace-raw-2026-09-29.txt` 是 raw 模式（自写 `EntityInputHandler`，绕开 gpui-kit 的 `Input`）下的一次真人输入日志，
因 `*.log` 被仓库忽略，改后缀后留档。

- 日志**不记录输入法身份**（`hkl=0x8040804` 对微软拼音与搜狗相同）。输入法身份来自测试者当场口头确认：
  该次日志是**微软拼音**（逐键 `WM_IME_COMPOSITION` 带 COMPSTR，随后 `replace_and_mark_text_in_range` 被调用，`marked` 随之更新）。
- 搜狗那次测试的原始日志已在重启程序时被覆盖，**没有留档**；"搜狗只在上屏时发 RESULTSTR、不走应用内预编辑"这一结论
  只有当时会话里的日志摘录作为依据，缺少可复核的原始文件。
- **未验证**：gpui-kit 高层 `Input` 在真人输入下的表现（kit 模式只有冒烟日志）。`bounds_for_range` 只记录了调用，未记录返回值，
  候选窗定位未验证。
