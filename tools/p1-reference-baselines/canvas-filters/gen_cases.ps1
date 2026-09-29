# 生成 cases.txt（黄金样本用例清单，Rust 测试与 C++ golden_main 共用）。
# 每行：name kind 参数...，格式见 golden_main.cpp 顶部与 snow-canvas-filters/tests/golden.rs。
# 用法：powershell -File gen_cases.ps1 [-Out cases.txt]
param([string]$Out = "$PSScriptRoot\cases.txt")
$ErrorActionPreference = "Stop"
$inv = [System.Globalization.CultureInfo]::InvariantCulture
function F($v) { ([double]$v).ToString("R", $inv) }
$lines = New-Object System.Collections.Generic.List[string]
$script:seed = 100
function NextSeed { $script:seed += 7; $script:seed }

# 参数尾巴：type strength block sigma radius dpr ox oy fs
function P($type, $strength, $block, $sigma, $radius, $dpr, $ox, $oy, $fs) {
    "$type $(F $strength) $(F $block) $(F $sigma) $(F $radius) $(F $dpr) $(F $ox) $(F $oy) $fs"
}

$smallSizes = @(@(1, 1), @(3, 2), @(7, 5), @(9, 9), @(17, 13), @(33, 20), @(67, 53))
$allSizes = $smallSizes + @(, @(130, 71))

# ---- apply（整图原地）----
foreach ($s in $allSizes) {
    $w = $s[0]; $h = $s[1]
    foreach ($fs in 0, 1) { if ($fs -eq 0) { $sv = $script:seed } else { $script:seed = $sv }
        # 马赛克：块大小 / dpr / 原点
        foreach ($m in @(@(1, 1, 0, 0), @(2, 1, 0, 0), @(3, 1, 5, 3), @(7, 1, -4, 9.6), @(16, 1, 0, 0), @(33, 1, 11, 2), @(10, 1.5, 3, 3))) {
            $lines.Add("mosaic_${w}x${h}_b$($m[0])_d$($m[1])_o$($m[2])_$($m[3])_fs$fs apply $w $h $(NextSeed) $(P 0 1 $m[0] 0 0 $m[1] $m[2] $m[3] $fs)")
        }
        # 模糊：覆盖各降采样档位
        foreach ($b in @(@(0, 1), @(0.5, 1), @(1, 1), @(1.9, 1), @(2, 1), @(3, 1), @(5, 1), @(7.9, 1), @(8, 1), @(15, 1), @(20, 1), @(40, 1), @(130, 1), @(200, 1), @(1.5, 2), @(9, 2))) {
            $lines.Add("blur_${w}x${h}_s$($b[0])_d$($b[1])_fs$fs apply $w $h $(NextSeed) $(P 1 1 0 $b[0] 0 $b[1] 0 0 $fs)")
        }
        # 灰度/反相：强度档
        foreach ($t in 2, 3) {
            foreach ($st in 0, 0.003, 0.5, 0.9, 1) {
                $lines.Add("color${t}_${w}x${h}_st${st}_fs$fs apply $w $h $(NextSeed) $(P $t $st 0 0 0 1 0 0 $fs)")
            }
        }
        # 浮雕
        foreach ($e in @(@(0, 1), @(1, 1), @(1, 0.3), @(2, 1), @(3.2, 0.7), @(40, 1))) {
            $lines.Add("emboss_${w}x${h}_r$($e[0])_st$($e[1])_fs$fs apply $w $h $(NextSeed) $(P 4 $e[1] 0 0 $e[0] 1 0 0 $fs)")
        }
    }
}

# ---- applyMasked ----
$maskSizes = @(@(9, 9), @(17, 13), @(33, 20), @(67, 53))
foreach ($s in $maskSizes) {
    $w = $s[0]; $h = $s[1]
    # 矩形（相对整图）：整图、内部、贴边、越界、极小
    $rects = @(@(0, 0, $w, $h), @(2, 1, ($w - 4), ($h - 2)), @(($w - 9), ($h - 5), 9, 5), @(-3, -2, ($w + 6), ($h + 4)), @(1, 1, 1, 1), @(0, 0, 8, 3))
    $ri = 0
    foreach ($r in $rects) {
        $ri++
        foreach ($fs in 0, 1) { if ($fs -eq 0) { $sv = $script:seed } else { $script:seed = $sv }
            foreach ($t in 0, 1, 2, 3, 4) {
                $par = switch ($t) {
                    0 { P 0 1 5 0 0 1 2 1 $fs }
                    1 { P 1 1 0 3 0 1 0 0 $fs }
                    2 { P 2 0.8 0 0 0 1 0 0 $fs }
                    3 { P 3 1 0 0 0 1 0 0 $fs }
                    4 { P 4 1 0 0 2 1 0 0 $fs }
                }
                # 遮罩覆盖整个目标（原点 0,0）
                $lines.Add("masked_${w}x${h}_r${ri}_t${t}_fs$fs masked $w $h $(NextSeed) $(NextSeed) 0 0 $w $h $($r[0]) $($r[1]) $($r[2]) $($r[3]) $par")
            }
        }
    }
    # 遮罩偏移且仅覆盖部分：覆盖足够 / 覆盖不足（返回 false）
    foreach ($fs in 0, 1) { if ($fs -eq 0) { $sv = $script:seed } else { $script:seed = $sv }
        foreach ($t in 0, 1, 2, 4) {
            $par = switch ($t) {
                0 { P 0 1 4 0 0 1 0 0 $fs }
                1 { P 1 1 0 2 0 1 0 0 $fs }
                2 { P 2 1 0 0 0 1 0 0 $fs }
                4 { P 4 0.6 0 0 1 1 0 0 $fs }
            }
            $lines.Add("maskedoff_${w}x${h}_t${t}_fs$fs masked $w $h $(NextSeed) $(NextSeed) 3 2 $($w - 3) $($h - 2) 4 3 $($w - 8) $($h - 6) $par")
            $lines.Add("maskedshort_${w}x${h}_t${t}_fs$fs masked $w $h $(NextSeed) $(NextSeed) 3 2 $($w - 6) $($h - 5) 0 0 $w $h $par")
        }
    }
}

# ---- applyRect / applyRegion ----
foreach ($s in $maskSizes) {
    $w = $s[0]; $h = $s[1]
    foreach ($fs in 0, 1) { if ($fs -eq 0) { $sv = $script:seed } else { $script:seed = $sv }
        foreach ($op in 0, 0.3, 1) {
            foreach ($t in 1, 2, 3, 4) {
                $par = switch ($t) {
                    1 { P 1 1 0 4 0 1 0 0 $fs }
                    2 { P 2 0.7 0 0 0 1 0 0 $fs }
                    3 { P 3 1 0 0 0 1 0 0 $fs }
                    4 { P 4 1 0 0 1 1 0 0 $fs }
                }
                $lines.Add("rect_${w}x${h}_op${op}_t${t}_fs$fs rect $w $h $(NextSeed) 2 1 $($w - 3) $($h - 2) $op $par")
            }
        }
        foreach ($t in 1, 2, 3, 4) {
            $par = switch ($t) {
                1 { P 1 1 0 6 0 1 0 0 $fs }
                2 { P 2 1 0 0 0 1 0 0 $fs }
                3 { P 3 0.5 0 0 0 1 0 0 $fs }
                4 { P 4 1 0 0 2 1 0 0 $fs }
            }
            $lines.Add("region_${w}x${h}_t${t}_fs$fs region $w $h $(NextSeed) 2 0 0 $([int]($w / 2)) $([int]($h / 2)) $([int]($w / 2) + 1) $([int]($h / 2) + 1) $([int]($w / 2) - 1) $([int]($h / 2) - 1) $par")
        }
    }
}
# 马赛克不支持 rect/region（返回 false）
$lines.Add("rect_mosaic_unsupported rect 17 13 5 0 0 17 13 1 $(P 0 1 4 0 0 1 0 0 0)")

# ---- blendOverSource ----
foreach ($s in $smallSizes) {
    foreach ($op in 0, 0.25, 0.5, 0.999, 1) {
        $lines.Add("blend_$($s[0])x$($s[1])_op$op blend $($s[0]) $($s[1]) $(NextSeed) $op")
    }
}

# ---- 模糊计划 / 采样半径 ----
foreach ($sg in 0, 0.4, 1, 1.99, 2, 3.9, 4, 7, 8, 16, 31.9, 32, 100, 128, 500) {
    foreach ($d in 1, 1.5, 2) { $lines.Add("plan_s${sg}_d$d plan $(F $sg) $(F $d)") }
}
foreach ($t in 0, 2, 3, 4) {
    foreach ($r in 0, 0.3, 1, 2.5) {
        foreach ($d in 1, 0.5, 2) { $lines.Add("samp_t${t}_r${r}_d$d samp $t 0 $(F $r) $(F $d)") }
    }
}

# ---- 画笔胶囊遮罩（AVX2 主体 + 标量尾部）----
# size seed stride begX endX begY endY ax ay bx by outer tileLeft tileTop
$pens = @(
    @(32, 32, 0, 32, 0, 32, 4.5, 4.5, 26.2, 20.7, 3.5, 0, 0),
    @(32, 40, 3, 30, 2, 29, 10, 10, 10, 10, 2.5, 0, 0),
    @(32, 32, 0, 32, 0, 32, 16, 16, 16, 16.4, 4.0, 5, 7),
    @(37, 37, 1, 36, 1, 36, 2.25, 30.5, 33.1, 3.9, 6.5, 100, 200),
    @(64, 64, 0, 64, 0, 64, -5, -5, 70, 70, 9.5, 0, 0),
    @(64, 68, 5, 61, 5, 61, 12.5, 40, 50, 12.5, 0.5, -20, -30),
    @(9, 12, 0, 9, 0, 9, 1, 1, 8, 8, 2, 0, 0),
    @(33, 33, 0, 33, 0, 33, 0.123, 32.987, 31.5, 0.01, 7.25, 0, 0)
)
$pi = 0
foreach ($p in $pens) {
    $pi++
    $lines.Add("pen_$pi pen $($p[0]) $(NextSeed) $($p[1]) $($p[2]) $($p[3]) $($p[4]) $($p[5]) $(F $p[6]) $(F $p[7]) $(F $p[8]) $(F $p[9]) $(F $p[10]) $($p[11]) $($p[12])")
}

Set-Content -Path $Out -Value $lines -Encoding ascii
Write-Host "已生成 $($lines.Count) 条用例 -> $Out"
