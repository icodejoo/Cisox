# 重复实测：端到端在 iGPU 上单次波动很大，这里对关键配置交错重复 3 次。
# 用法: run-repeat.ps1 -Out <目录> [-Only r1,r2,...]
param(
    [string]$Out = "C:\Users\jelon\AppData\Local\Temp\claude\E--workspaces-Cisox\e9c32797-6599-4c85-bd8b-95c077321832\scratchpad\spike\data",
    [string[]]$Only = @(),
    [int]$Reps = 3
)
$ErrorActionPreference = "Continue"
$exe = Join-Path $PSScriptRoot "target\release\recording-convert-spike.exe"
New-Item -ItemType Directory -Force $Out | Out-Null
$frames = 600; $warm = 60
function Run([string]$file, [string[]]$a) {
    $line = & $exe @a 2>&1 | Select-Object -Last 1
    Add-Content -Path (Join-Path $Out $file) -Value $line -Encoding utf8
    Write-Host ("[{0}] {1}" -f $file, ($a -join " "))
    Start-Sleep -Milliseconds 800
}
function Want([string]$g) { return ($Only.Count -eq 0) -or ($Only -contains $g) }
$cfgs = @(@(1, "medium"), @(4, "veryfast"))

if (Want "r1") {   # 主对比：原生输出尺寸
    foreach ($rep in 1..$Reps) { foreach ($res in "1440", "1080") { foreach ($content in "noisy", "desktop") { foreach ($c in $cfgs) { foreach ($m in "vp_base", "vp_opt", "ps", "cs") {
        Run "e2e_rep.jsonl" @("e2e", "--res", $res, "--method", $m, "--content", $content, "--async", $c[0], "--preset", $c[1], "--frames", $frames, "--warmup", $warm)
    } } } } }
}
if (Want "r2") {   # QSV 参数扫描（vp_opt）
    foreach ($rep in 1..$Reps) { foreach ($res in "1440", "1080") { foreach ($content in "noisy", "desktop") { foreach ($c in @(@(2, "medium"), @(4, "medium"), @(1, "veryfast"), @(2, "veryfast"))) {
        Run "e2e_sweep.jsonl" @("e2e", "--res", $res, "--method", "vp_opt", "--content", $content, "--async", $c[0], "--preset", $c[1], "--frames", $frames, "--warmup", $warm)
    } } } }
}
if (Want "r3") {   # 1440 输入缩放到 1080 输出（上游默认输出上限 1080p）
    foreach ($rep in 1..$Reps) { foreach ($c in $cfgs) { foreach ($m in "vp_opt", "ps") {
        Run "e2e_scale.jsonl" @("e2e", "--res", "1440", "--out", "1080", "--method", $m, "--content", "desktop", "--async", $c[0], "--preset", $c[1], "--frames", $frames, "--warmup", $warm)
    } } }
    foreach ($m in "vp_opt", "ps") { foreach ($rep in 1..2) { Run "convert_scale.jsonl" @("convert", "--res", "1440", "--out", "1080", "--method", $m, "--frames", $frames, "--warmup", $warm) } }
}
if (Want "r4") {   # 帧池纹理标志消融
    foreach ($rep in 1..$Reps) { foreach ($content in "noisy", "desktop") { foreach ($c in $cfgs) { foreach ($b in "rt", "rt_ve") {
        Run "e2e_bind_rep.jsonl" @("e2e", "--res", "1440", "--method", "vp_opt", "--bind", $b, "--content", $content, "--async", $c[0], "--preset", $c[1], "--frames", $frames, "--warmup", $warm)
    } } } }
}
if (Want "r5") {   # MF 端到端
    foreach ($rep in 1..$Reps) { foreach ($res in "1440", "1080") { foreach ($content in "noisy", "desktop") { foreach ($d in 0, 1) {
        Run "mf_e2e_rep.jsonl" @("mf-e2e", "--res", $res, "--direct", $d, "--content", $content, "--frames", $frames, "--warmup", $warm)
    } } } }
}

if (Want "r6") {   # 单次 Blt 多图层（把覆盖层/光标并入同一 Blt）
    foreach ($rep in 1..2) { foreach ($res in "1440", "1080") { foreach ($m in "vp_multi", "vp_multi_tile") {
        Run "convert_multi.jsonl" @("convert", "--res", $res, "--method", $m, "--frames", $frames, "--warmup", $warm)
    } } }
    foreach ($rep in 1..$Reps) { foreach ($res in "1440", "1080") { foreach ($content in "noisy", "desktop") { foreach ($c in $cfgs) { foreach ($m in "vp_multi", "vp_multi_tile") {
        Run "e2e_multi.jsonl" @("e2e", "--res", $res, "--method", $m, "--content", $content, "--async", $c[0], "--preset", $c[1], "--frames", $frames, "--warmup", $warm)
    } } } } }
}
