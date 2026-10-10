# Cisox 自有构建脚本（build-snow-*.ps1）共用的小函数，用法: . (Join-Path $PSScriptRoot "common.ps1")

# 不在开发者 shell 时，从 vcvars64.bat 导入 MSVC 环境（INCLUDE/LIB/link.exe）；已有 INCLUDE 则什么都不做。
# -PrependVswhereDir: 额外把 vswhere 所在目录放到 PATH 最前（recorder/translator 原行为）。
function Import-SnowVsEnvironment([switch]$PrependVswhereDir) {
    if ($env:INCLUDE) { return }
    $vswhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio/Installer/vswhere.exe"
    $vsPath = & $vswhere -latest -products * -property installationPath
    $vcvars = Join-Path $vsPath "VC/Auxiliary/Build/vcvars64.bat"
    if (-not (Test-Path $vcvars)) { throw "找不到 vcvars64.bat: $vcvars" }
    if ($PrependVswhereDir) { $env:Path = "$(Split-Path $vswhere);$env:Path" }
    cmd /c "`"$vcvars`" >nul 2>&1 && set" | ForEach-Object {
        if ($_ -match "^([^=]+)=(.*)$") { Set-Item -Path "Env:$($Matches[1])" -Value $Matches[2] }
    }
}

# 按 -Clippy / -Test / 默认构建 三选一调用 cargo，失败时抛错；release 档自动带 --release。
function Invoke-SnowCargo([string]$Manifest, [string]$Profile, [bool]$Test, [bool]$Clippy) {
    [string[]]$profileArgs = if ($Profile -eq "release") { @("--release") } else { @() }
    if ($Clippy) {
        cargo clippy --manifest-path $Manifest @profileArgs --all-targets -- -D warnings
    }
    elseif ($Test) {
        cargo test --manifest-path $Manifest @profileArgs
    }
    else {
        cargo build --manifest-path $Manifest @profileArgs
    }
    if ($LASTEXITCODE -ne 0) { throw "cargo 失败，退出码 $LASTEXITCODE" }
}

# 找 libclang（bindgen 需要）：LIBCLANG_PATH 优先，其次仓库 .tools/llvm、常见安装位置；找到后设环境变量并把目录追加到 PATH 末尾。
function Set-SnowLibclangPath([string]$RepoRoot) {
    if (-not [string]::IsNullOrWhiteSpace($env:LIBCLANG_PATH)) { return }
    $candidates = @(
        (Join-Path $RepoRoot ".tools/llvm/bin"),
        "C:\Program Files\LLVM\bin",
        "C:\tools\msys64\mingw64\bin"
    )
    $found = $candidates | Where-Object { Test-Path (Join-Path $_ "libclang.dll") } | Select-Object -First 1
    if (-not $found) { throw "找不到 libclang.dll，请设置 LIBCLANG_PATH" }
    $env:LIBCLANG_PATH = $found
    # libclang 的依赖 DLL 与其同目录，放到 PATH 末尾以便加载（不覆盖 MSVC 工具）
    $env:Path = "$env:Path;$found"
}
