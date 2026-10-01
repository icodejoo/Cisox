#!/usr/bin/env python3
"""跨屏录制接缝对齐检查：在 `--load grid` 成品里找白色竖线，与期望列位置比较。

用法:
    python check_seam.py 成品.mp4 [--x0 1600] [--seam-x 2560] [--tol 1] [--seam-tol 0.25] [--frames 8]

前提: 选区 1920x1080、输出与源尺寸一致（不做缩放后的像素级断言）；夹具在桌面绝对 x 为 64 倍数处画 1px 白线。
退出码: 0=通过，1=不通过，2=运行出错。
依赖: Python 3 标准库 + 系统 ffmpeg / ffprobe（默认 C:\\ProgramData\\chocolatey\\bin，可用 --ffmpeg-dir 覆盖）。
"""

import argparse
import shutil
import subprocess
import sys
from dataclasses import dataclass, field
from typing import List, Optional, Sequence, Tuple

# 夹具网格竖线的桌面 x 间距（与夹具 content::GRID_PITCH 一致）。
GRID_PITCH = 64
# 判定为白线的灰度阈值（限幅/全范围的中点都适用）。
LINE_THRESHOLD = 128
# 接缝附近的关注半宽（列）。
SEAM_ZONE = 8
# 期望线与实测线配对的最大距离（列）。
MATCH_RADIUS = 4
# 默认一般容差（列）。
DEFAULT_TOL = 1.0
# 默认接缝附近容差（列）：接缝处要求像素级精确，仅留给编码模糊的亚像素余量。
DEFAULT_SEAM_TOL = 0.25
# 线宽（列）达到该值视为加宽/重复。
MAX_LINE_WIDTH = 2
# 采样行位置（占高度比例）：避开顶部序号条与 50% 处的横线。
ROW_FRACTIONS = (0.25, 0.35, 0.65, 0.8)
# 默认 ffmpeg 目录。
DEFAULT_FFMPEG_DIR = r"C:\ProgramData\chocolatey\bin"
# 默认选区左边界与接缝（桌面 x）。
DEFAULT_X0 = 1600
DEFAULT_SEAM_X = 2560
# 跳过的开头帧数与抽帧步长（避开编码器启动期）。
SKIP_FRAMES = 10
FRAME_STEP = 10
# 报告中最多列出的问题条数。
MAX_ISSUES_SHOWN = 20


@dataclass
class Line:
    """一条实测竖线。"""

    # 加权中心列（浮点）。
    center: float
    # 线宽（列）。
    width: int


@dataclass
class Report:
    """比较结果。"""

    # 期望线数。
    expected: int = 0
    # 实测线数。
    found: int = 0
    # 最大位置误差（列）。
    max_err: float = 0.0
    # 接缝附近（±SEAM_ZONE）最大位置误差（列）。
    seam_err: float = 0.0
    # 问题描述。
    issues: List[str] = field(default_factory=list)

    @property
    def passed(self) -> bool:
        """是否无任何问题。"""
        return not self.issues


def find_lines(row: Sequence[int], threshold: int = LINE_THRESHOLD) -> List[Line]:
    """在一行灰度里找出连续的亮列，每段视为一条线。

    参数:
        row: 一行灰度值。
        threshold: 亮列阈值。
    返回:
        按位置排序的线（中心为亮度加权）。
    示例:
        >>> [(l.center, l.width) for l in find_lines([0, 0, 255, 0, 255, 255, 0])]
        [(2.0, 1), (4.5, 2)]
    """
    lines: List[Line] = []
    start: Optional[int] = None
    for i in range(len(row) + 1):
        bright = i < len(row) and row[i] >= threshold
        if bright and start is None:
            start = i
        elif not bright and start is not None:
            weights = row[start:i]
            total = sum(weights)
            center = sum((start + k) * w for k, w in enumerate(weights)) / total
            lines.append(Line(center, i - start))
            start = None
    return lines


def expected_columns(width: int, x0: int, pitch: int = GRID_PITCH) -> List[int]:
    """期望的竖线输出列：桌面绝对 x 为 pitch 整数倍，减去选区左边界。

    参数:
        width: 输出宽度。
        x0: 选区左边界（桌面 x）。
        pitch: 线间距。
    返回:
        升序列号列表。
    示例:
        >>> expected_columns(200, 1600)
        [0, 64, 128, 192]
    """
    first = -(-x0 // pitch) * pitch
    return [x - x0 for x in range(first, x0 + width, pitch)]


def compare_row(
    row: Sequence[int],
    expected: Sequence[int],
    seam_col: int,
    tol: float = DEFAULT_TOL,
    seam_tol: float = DEFAULT_SEAM_TOL,
) -> Report:
    """比较一行实测线与期望列。

    参数:
        row: 一行灰度值。
        expected: 期望列。
        seam_col: 接缝所在输出列（= 接缝桌面 x - 选区 x0）。
        tol: 一般容差（列）。
        seam_tol: 接缝 ±SEAM_ZONE 内的容差（列）。
    返回:
        比较报告；缺失/多余/错位/加宽都记入 issues。
    示例:
        >>> compare_row([0, 255, 0, 0, 255, 0], [1, 4], 4).passed
        True
    """
    found = find_lines(row)
    rep = Report(expected=len(expected), found=len(found))
    unused = list(range(len(found)))
    for exp in expected:
        in_seam = abs(exp - seam_col) <= SEAM_ZONE
        tag = "（接缝附近）" if in_seam else ""
        cand = [i for i in unused if abs(found[i].center - exp) <= MATCH_RADIUS]
        if not cand:
            rep.issues.append(f"缺失: 列 {exp}{tag}")
            continue
        best = min(cand, key=lambda i: abs(found[i].center - exp))
        unused.remove(best)
        line = found[best]
        err = abs(line.center - exp)
        limit = seam_tol if in_seam else tol
        rep.max_err = max(rep.max_err, err)
        if in_seam:
            rep.seam_err = max(rep.seam_err, err)
        if err > limit:
            rep.issues.append(f"错位: 期望列 {exp} 实测 {line.center:.2f}（误差 {err:.2f} > {limit}）{tag}")
        if in_seam and line.width >= MAX_LINE_WIDTH:
            rep.issues.append(f"加宽/重复: 列 {exp} 线宽 {line.width}{tag}")
    for i in unused:
        tag = "（接缝附近）" if abs(found[i].center - seam_col) <= SEAM_ZONE else ""
        rep.issues.append(f"多余: 列 {found[i].center:.2f}{tag}")
    return rep


def find_tool(name: str, extra_dir: Optional[str]) -> str:
    """定位 ffmpeg/ffprobe 可执行文件。

    参数:
        name: 工具名（不含扩展名）。
        extra_dir: 额外搜索目录。
    返回:
        可执行路径；找不到抛 FileNotFoundError。
    """
    found = shutil.which(name, path=extra_dir) if extra_dir else None
    found = found or shutil.which(name)
    if not found:
        raise FileNotFoundError(f"找不到 {name}，请加入 PATH 或用 --ffmpeg-dir 指定")
    return found


def probe_size(ffprobe: str, path: str) -> Tuple[int, int]:
    """读取视频宽高。

    参数:
        ffprobe: ffprobe 路径。
        path: 成品路径。
    返回:
        (宽, 高)。
    """
    cmd = [ffprobe, "-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width,height", "-of", "csv=p=0", path]
    w, h = subprocess.run(cmd, capture_output=True, text=True, check=True).stdout.strip().split(",")[:2]
    return int(w), int(h)


def decode_gray_frames(ffmpeg: str, path: str, width: int, height: int, count: int) -> List[bytes]:
    """抽取若干帧为原始灰度（跳过开头、按步长抽样）。

    参数:
        ffmpeg: ffmpeg 路径。
        path: 成品路径。
        width、height: 视频尺寸。
        count: 最多抽取帧数。
    返回:
        每帧 width*height 字节；帧数可能少于 count。
    """
    select = f"select=gte(n\\,{SKIP_FRAMES})*not(mod(n\\,{FRAME_STEP}))"
    cmd = [ffmpeg, "-v", "error", "-i", path, "-vf", select, "-fps_mode", "passthrough", "-frames:v", str(count), "-f", "rawvideo", "-pix_fmt", "gray", "-"]
    raw = subprocess.run(cmd, capture_output=True, check=True).stdout
    size = width * height
    return [raw[i * size:(i + 1) * size] for i in range(len(raw) // size)]


def check_frames(frames: Sequence[bytes], width: int, height: int, x0: int, seam_x: int, tol: float, seam_tol: float) -> Tuple[Report, List[str]]:
    """对多帧多行逐一比较并汇总。

    参数:
        frames: 灰度帧。
        width、height: 视频尺寸。
        x0: 选区左边界（桌面 x）。
        seam_x: 接缝桌面 x。
        tol、seam_tol: 一般/接缝容差。
    返回:
        (汇总报告, 每帧接缝附近实测线位置的描述行)。
    """
    expected = expected_columns(width, x0)
    seam_col = seam_x - x0
    total = Report(expected=len(expected))
    seam_lines: List[str] = []
    for fi, frame in enumerate(frames):
        for ri, frac in enumerate(ROW_FRACTIONS):
            y = int(height * frac)
            row = frame[y * width:(y + 1) * width]
            rep = compare_row(row, expected, seam_col, tol, seam_tol)
            total.found = rep.found
            total.max_err = max(total.max_err, rep.max_err)
            total.seam_err = max(total.seam_err, rep.seam_err)
            total.issues += [f"帧#{fi} 行 {y}: {msg}" for msg in rep.issues]
            if ri == 0:
                near = [f"{ln.center:.1f}" for ln in find_lines(row) if abs(ln.center - seam_col) <= SEAM_ZONE]
                seam_lines.append(f"帧#{fi} 接缝列 {seam_col} 附近实测线: {', '.join(near) or '无'}")
    return total, seam_lines


def main(argv: Optional[List[str]] = None) -> int:
    """命令行入口。

    参数:
        argv: 参数列表（缺省取 sys.argv）。
    返回:
        0=通过，1=不通过，2=出错。
    """
    ap = argparse.ArgumentParser(description="跨屏录制接缝对齐检查")
    ap.add_argument("video", help="成品路径（1920x1080 选区的 grid 录制）")
    ap.add_argument("--x0", type=int, default=DEFAULT_X0, help="选区左边界（桌面 x）")
    ap.add_argument("--seam-x", type=int, default=DEFAULT_SEAM_X, help="接缝桌面 x")
    ap.add_argument("--tol", type=float, default=DEFAULT_TOL, help="一般位置容差（列）")
    ap.add_argument("--seam-tol", type=float, default=DEFAULT_SEAM_TOL, help="接缝 ±8 列内位置容差（列）")
    ap.add_argument("--frames", type=int, default=8, help="抽取帧数")
    ap.add_argument("--ffmpeg-dir", default=DEFAULT_FFMPEG_DIR, help="ffmpeg/ffprobe 所在目录")
    args = ap.parse_args(argv)
    try:
        ffmpeg = find_tool("ffmpeg", args.ffmpeg_dir)
        ffprobe = find_tool("ffprobe", args.ffmpeg_dir)
        width, height = probe_size(ffprobe, args.video)
        frames = decode_gray_frames(ffmpeg, args.video, width, height, args.frames)
    except (OSError, ValueError, subprocess.CalledProcessError) as e:
        print(f"出错: {e}", file=sys.stderr)
        return 2
    if not frames:
        print("出错: 没有解出任何帧", file=sys.stderr)
        return 2
    rep, seam_lines = check_frames(frames, width, height, args.x0, args.seam_x, args.tol, args.seam_tol)
    print(f"seam-check: {width}x{height} 帧数={len(frames)} 期望线数={rep.expected} 找到线数={rep.found}（末帧末行）")
    print(f"最大位置误差={rep.max_err:.2f} 列  接缝附近最大误差={rep.seam_err:.2f} 列（容差 {args.tol} / 接缝 {args.seam_tol}）")
    if (width, height) != (1920, 1080):
        print(f"警告: 输出尺寸 {width}x{height} 不是 1920x1080，可能被缩放，结果不可靠")
    print("\n".join(seam_lines[:2]))
    for msg in rep.issues[:MAX_ISSUES_SHOWN]:
        print("  " + msg)
    if len(rep.issues) > MAX_ISSUES_SHOWN:
        print(f"  ...另有 {len(rep.issues) - MAX_ISSUES_SHOWN} 条")
    print("PASS" if rep.passed else "FAIL")
    return 0 if rep.passed else 1


if __name__ == "__main__":
    sys.exit(main())
