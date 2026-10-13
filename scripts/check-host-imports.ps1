# 托盘宿主 exe 导入表闸门（方案 docs/design/tray-host-split.md §3 第三层、§12.6）。
#
# 检查宿主 cisox.exe 的 PE 导入表（静态 + 延迟）只含白名单内的系统 DLL，没有 D3D / DXGI / DWrite /
# UIA / 图像编解码 / 输入法这类会在进程启动时被加载、涨内存的 DLL。不依赖 dumpbin / MSVC 环境：
# 解析器与白名单在 snow-shot-rs/tools/workspace-guard（imports.rs），这里只是构建 + 调用的薄包装。
#
# 用法（仓库任意目录）:
#   scripts/check-host-imports.ps1                    # 检查已有的 release 产物
#   scripts/check-host-imports.ps1 -Build             # 先 cargo build --release -p snow-tray-host 再检查
#   scripts/check-host-imports.ps1 -Build -WithUi     # 宿主与 UI 两个包同一条命令构建（CI 用，贴近真实打包）
#   scripts/check-host-imports.ps1 -Exe <路径> -List  # 只列出导入的 DLL，不判定（定稿白名单时用）
#
# 产物目录：设了环境变量 CARGO_TARGET_DIR 就用它（相对路径按 snow-shot-rs/ 解析），否则是仓库根的
# build\cargo（见根 .cargo/config.toml）。
#
# 退出码：0 通过；1 有违规；2 参数 / 文件 / 构建问题。
[CmdletBinding()]
param(
    [string]$Exe = "",
    [switch]$Build,
    [switch]$WithUi,
    [switch]$List
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$workspace = Join-Path $repoRoot "snow-shot-rs"
if (-not $Exe) {
    # Windows PowerShell 5.1（.NET Framework）没有 GetFullPath(path, basePath) 重载，相对路径手动拼
    $targetDir = if ($env:CARGO_TARGET_DIR) {
        if ([System.IO.Path]::IsPathRooted($env:CARGO_TARGET_DIR)) { $env:CARGO_TARGET_DIR }
        else { Join-Path $workspace $env:CARGO_TARGET_DIR }
    }
    else {
        Join-Path $repoRoot "build\cargo"
    }
    $Exe = Join-Path $targetDir "release\cisox.exe"
}

Push-Location $workspace
try {
    if ($Build) {
        $packages = @("-p", "snow-tray-host")
        if ($WithUi) { $packages += @("-p", "snow-shot") }
        Write-Host ">> cargo build --release $($packages -join ' ')"
        cargo build --release @packages
        if ($LASTEXITCODE -ne 0) { Write-Error "宿主构建失败"; exit 2 }
    }
    if (-not (Test-Path -LiteralPath $Exe)) {
        Write-Error "找不到宿主 exe: $Exe（先加 -Build 构建）"
        exit 2
    }
    $item = Get-Item -LiteralPath $Exe
    Write-Host ("宿主 exe: {0}  {1:N0} 字节  {2}" -f $item.FullName, $item.Length, $item.LastWriteTime)
    $checkerArgs = @("run", "-q", "-p", "workspace-guard", "--bin", "check-host-imports", "--", $item.FullName)
    if ($List) { $checkerArgs += "--list" }
    & cargo @checkerArgs
    exit $LASTEXITCODE
}
finally {
    Pop-Location
}
