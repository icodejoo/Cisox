# 构建/运行环境（镜像 scripts/build-snow-recorder.ps1）。用法: build.ps1 <cargo 参数...>
param([Parameter(ValueFromRemainingArguments = $true)][string[]]$CargoArgs)
$ErrorActionPreference = "Continue"
$repoRoot = "E:\workspaces\Cisox"
$env:FFMPEG_DIR = Join-Path $repoRoot ".tools/vcpkg/installed/static/x64-windows-static"
$env:VCPKGRS_TRIPLET = "x64-windows-static"
$env:VCPKGRS_DYNAMIC = "0"
$env:LIBCLANG_PATH = "C:\tools\msys64\mingw64\bin"
$env:Path = "$env:LIBCLANG_PATH;$env:Path"
if (-not $env:INCLUDE) {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio/Installer/vswhere.exe"
    $vsPath = & $vswhere -latest -products * -property installationPath
    $vcvars = Join-Path $vsPath "VC/Auxiliary/Build/vcvars64.bat"
    $env:Path = "$(Split-Path $vswhere);$env:Path"
    cmd /c "`"$vcvars`" >nul 2>&1 && set" | ForEach-Object {
        if ($_ -match "^([^=]+)=(.*)$") { Set-Item -Path "Env:$($Matches[1])" -Value $Matches[2] }
    }
}
$env:RUSTFLAGS = "-C target-feature=+crt-static"
$env:CARGO_BUILD_JOBS = "4"
Set-Location $PSScriptRoot
& cargo @CargoArgs
exit $LASTEXITCODE
