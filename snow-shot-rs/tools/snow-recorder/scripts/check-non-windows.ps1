# 非 Windows 目标的编译检查：生成一个只含跨平台模块的临时 crate，对 x86_64-unknown-linux-gnu 做 cargo check（含测试代码）。
#
# 为什么不直接 check 整个 snow-recorder：上游 snow-crates 的 C 依赖（zstd-sys、ffmpeg-sys 等）在 Windows 上没有 Linux 交叉编译器，
# 整包 check 会卡在它们的 build.rs 上。这里 check 的是本工具里"与平台无关"的全部模块
# （traits/流水线/时间线/几何/设置/后端装配/动图尾段修补），并用一个桩代替软编后端（上游会话），
# 以验证 `#[cfg(windows)]` 的隔离边界：非 Windows 上只编译软编装配路径，不会引用任何 win 模块。
#
# 用法: scripts/check-non-windows.ps1 [-Target x86_64-unknown-linux-gnu]
[CmdletBinding(PositionalBinding = $false)]
param([string]$Target = "x86_64-unknown-linux-gnu")
$ErrorActionPreference = "Continue"
$toolRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$repoRoot = (Resolve-Path (Join-Path $toolRoot "../../..")).Path
$src = Join-Path $toolRoot "src"
$work = Join-Path $repoRoot "build/recorder-nonwin-check"
New-Item -ItemType Directory -Force -Path (Join-Path $work "src") | Out-Null
$fwd = { param($p) $p.Replace("\", "/") }
$cursor = & $fwd (Join-Path $repoRoot "snow-crates/crates/snow-cursor")
$protocol = & $fwd (Join-Path $repoRoot "snow-shot-rs/crates/snow-recorder-protocol")
@"
[workspace]

[package]
name = "snow-recorder-nonwin-check"
version = "0.0.0"
edition = "2024"

[dependencies]
snow-cursor = { path = "$cursor" }
snow-recorder-protocol = { path = "$protocol" }
"@ | Set-Content -Encoding utf8 (Join-Path $work "Cargo.toml")
$common = "backend", "clock", "frametrace", "geom", "os", "pipeline", "settings", "timeline"
# 第一遍：含 tailfix 的生产代码；第二遍：带测试代码（tailfix 的测试要用上游 crate 造文件，不在此检查范围内）
$passes = @(
    @{ modules = $common + "tailfix"; args = @("check") },
    @{ modules = $common; args = @("check", "--tests") }
)
$env:CARGO_TARGET_DIR = Join-Path $work "target"
Push-Location $work
try {
    foreach ($pass in $passes) {
        $lines = @("//! 自动生成：非 Windows 编译检查入口（见 scripts/check-non-windows.ps1）。", "#![allow(dead_code)]")
        foreach ($m in $pass.modules) { $lines += "#[path = `"$(& $fwd (Join-Path $src "$m.rs"))`"]", "mod $m;" }
        $lines += @"
/// 软编后端桩：真实实现包装上游 DirectRecordingSession（需要 Windows 上的 snow-crates 构建环境）。
mod soft {
    use crate::backend::RecordingBackend;
    pub fn start_software(_request: &snow_recorder_protocol::StartRequest, _partial: std::path::PathBuf, _upstream_gpu: bool) -> Result<Box<dyn RecordingBackend>, String> {
        Err("stub".into())
    }
}
fn main() {}
"@
        $lines | Set-Content -Encoding utf8 (Join-Path $work "src/main.rs")
        cargo @($pass.args) --target $Target
        if ($LASTEXITCODE -ne 0) { throw "非 Windows 编译检查失败（$($pass.args -join ' ')，退出码 $LASTEXITCODE）" }
    }
    "非 Windows 编译检查通过: $Target"
}
finally { Pop-Location }
