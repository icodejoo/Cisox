# 用 MSVC 编译真实 C++ 滤镜内核 + Qt 兼容层，产出 golden_main.exe。
# 用法：powershell -File build.ps1 [-Out <输出目录>] [-Fp precise|strict]
param(
    [string]$Out = "$PSScriptRoot\build",
    [string]$Fp = "precise",
    [string]$VcVars = "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat"
)
$ErrorActionPreference = "Stop"
$src = Resolve-Path "$PSScriptRoot\..\..\..\snow_draw_engine_qt\src\rendering"
New-Item -ItemType Directory -Force $Out | Out-Null
$exe = Join-Path $Out "golden_main.exe"
$cl = "cl /nologo /std:c++17 /O2 /EHsc /arch:AVX2 /fp:$Fp /W3 /I`"$PSScriptRoot\qt_shim`" /I`"$src`" " +
    "/Fo`"$Out\\`" /Fe`"$exe`" " +
    "`"$PSScriptRoot\golden_main.cpp`" `"$PSScriptRoot\diagnostics_stub.cpp`" " +
    "`"$src\snow_canvas_filter_render.cpp`" `"$src\snow_canvas_filter_avx2.cpp`" `"$src\snow_canvas_pen_mask_avx2.cpp`""
cmd /c "`"$VcVars`" >nul && $cl"
if ($LASTEXITCODE -ne 0) { throw "编译失败" }
Write-Host "已生成 $exe"
