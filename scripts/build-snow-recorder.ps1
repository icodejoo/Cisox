# 构建录制工作进程 snow-recorder（独立 workspace，静态 CRT + 静态 FFmpeg）。
# 用法: scripts/build-snow-recorder.ps1 [-Profile release|debug] [-Test] [-Clippy]
# 产物: snow-shot-rs/tools/snow-recorder/target/<profile>/snow-recorder.exe
param(
    [ValidateSet("release", "debug")][string]$Profile = "release",
    [switch]$Test,
    [switch]$Clippy
)
$ErrorActionPreference = "Continue"
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$manifest = Join-Path $repoRoot "snow-shot-rs/tools/snow-recorder/Cargo.toml"

# FFmpeg：默认使用仓库 vcpkg 的静态三元组（/MT）
if ([string]::IsNullOrWhiteSpace($env:FFMPEG_DIR)) {
    $env:FFMPEG_DIR = Join-Path $repoRoot ".tools/vcpkg/installed/static/x64-windows-static"
}
if (-not (Test-Path (Join-Path $env:FFMPEG_DIR "lib/avcodec.lib"))) {
    throw "FFMPEG_DIR 下缺少 lib/avcodec.lib: $($env:FFMPEG_DIR)"
}
$env:VCPKGRS_TRIPLET = "x64-windows-static"
$env:VCPKGRS_DYNAMIC = "0"

# libclang（bindgen 需要）：环境变量优先，其次常见安装位置
if ([string]::IsNullOrWhiteSpace($env:LIBCLANG_PATH)) {
    $candidates = @(
        (Join-Path $repoRoot ".tools/llvm/bin"),
        "C:\Program Files\LLVM\bin",
        "C:\tools\msys64\mingw64\bin"
    )
    $found = $candidates | Where-Object { Test-Path (Join-Path $_ "libclang.dll") } | Select-Object -First 1
    if (-not $found) { throw "找不到 libclang.dll，请设置 LIBCLANG_PATH" }
    $env:LIBCLANG_PATH = $found
    # libclang 的依赖 DLL 与其同目录，放到 PATH 末尾以便加载（不覆盖 MSVC 工具）
    $env:Path = "$env:Path;$found"
}

# MSVC 环境（INCLUDE/LIB/link.exe）：不在开发者 shell 时自动导入
if (-not $env:INCLUDE) {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio/Installer/vswhere.exe"
    $vsPath = & $vswhere -latest -products * -property installationPath
    $vcvars = Join-Path $vsPath "VC/Auxiliary/Build/vcvars64.bat"
    if (-not (Test-Path $vcvars)) { throw "找不到 vcvars64.bat: $vcvars" }
    $env:Path = "$(Split-Path $vswhere);$env:Path"
    cmd /c "`"$vcvars`" >nul 2>&1 && set" | ForEach-Object {
        if ($_ -match "^([^=]+)=(.*)$") { Set-Item -Path "Env:$($Matches[1])" -Value $Matches[2] }
    }
}

# 静态 CRT 与静态 FFmpeg 匹配；独立 target 目录避免与其它 workspace 混用
$env:RUSTFLAGS = "-C target-feature=+crt-static"
$env:CARGO_TARGET_DIR = Join-Path $repoRoot "snow-shot-rs/tools/snow-recorder/target"

[string[]]$profileArgs = if ($Profile -eq "release") { @("--release") } else { @() }
if ($Clippy) {
    cargo clippy --manifest-path $manifest @profileArgs --all-targets -- -D warnings
}
elseif ($Test) {
    cargo test --manifest-path $manifest @profileArgs
}
else {
    cargo build --manifest-path $manifest @profileArgs
}
if ($LASTEXITCODE -ne 0) { throw "cargo 失败，退出码 $LASTEXITCODE" }
