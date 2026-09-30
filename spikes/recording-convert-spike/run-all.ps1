# 全量实测驱动：按顺序运行所有方案，结果以 JSON 行写入 $Out。
# 用法: run-all.ps1 -Out <目录> [-Quick]
param(
    [string]$Out = "C:\Users\jelon\AppData\Local\Temp\claude\E--workspaces-Cisox\e9c32797-6599-4c85-bd8b-95c077321832\scratchpad\spike\data",
    [switch]$Quick,
    [string[]]$Only = @()
)
$ErrorActionPreference = "Continue"
$exe = Join-Path $PSScriptRoot "target\release\recording-convert-spike.exe"
New-Item -ItemType Directory -Force $Out | Out-Null
$frames = if ($Quick) { 120 } else { 600 }
$warm = if ($Quick) { 20 } else { 60 }

# 运行一次并把输出行追加到文件
function Run([string]$file, [string[]]$a) {
    $line = & $exe @a 2>&1 | Select-Object -Last 1
    Add-Content -Path (Join-Path $Out $file) -Value $line -Encoding utf8
    Write-Host ("[{0}] {1} -> {2}" -f $file, ($a -join " "), ($line.ToString().Substring(0, [Math]::Min(110, $line.ToString().Length))))
}
function Want([string]$g) { return ($Only.Count -eq 0) -or ($Only -contains $g) }

if (Want "env") {
    Run "env.jsonl" @("info"); Run "env.jsonl" @("dxgi-list"); Run "env.jsonl" @("mf-probe")
    Run "verify.jsonl" @("verify", "--res", "1080"); Run "verify.jsonl" @("verify", "--res", "1440")
}
if (Want "convert") {
    foreach ($rep in 1..2) {
        foreach ($res in "1440", "1080") {
            foreach ($m in "vp_base", "vp_opt_rt", "vp_opt", "ps", "cs") {
                Run "convert.jsonl" @("convert", "--res", $res, "--method", $m, "--frames", $frames, "--warmup", $warm)
            }
            Run "convert.jsonl" @("mf-convert", "--res", $res, "--frames", $frames, "--warmup", $warm)
        }
    }
}
if (Want "chain") {
    foreach ($res in "1440", "1080") { Run "chain.jsonl" @("chain", "--res", $res, "--frames", $frames, "--warmup", $warm) }
}
if (Want "e2e") {
    foreach ($res in "1440", "1080") {
        foreach ($m in "vp_base", "vp_opt", "ps", "cs") {
            foreach ($p in "medium", "veryfast") {
                foreach ($d in 1, 2, 4) {
                    Run "e2e.jsonl" @("e2e", "--res", $res, "--method", $m, "--async", $d, "--preset", $p, "--frames", $frames, "--warmup", $warm)
                }
            }
        }
    }
    # NV12 池纹理标志消融：RENDER_TARGET 与 RENDER_TARGET|VIDEO_ENCODER
    foreach ($b in "rt", "rt_ve") {
        foreach ($cfg in @(@(1, "medium"), @(4, "veryfast"))) {
            Run "e2e_bind.jsonl" @("e2e", "--res", "1440", "--method", "vp_opt", "--bind", $b, "--async", $cfg[0], "--preset", $cfg[1], "--frames", $frames, "--warmup", $warm)
        }
    }
}
if (Want "mfe2e") {
    foreach ($res in "1440", "1080") {
        Run "mf_e2e.jsonl" @("mf-e2e", "--res", $res, "--frames", $frames, "--warmup", $warm)
        Run "mf_e2e.jsonl" @("mf-e2e", "--res", $res, "--direct", 1, "--frames", $frames, "--warmup", $warm)
    }
}
