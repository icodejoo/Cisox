#!/usr/bin/env python3
"""跨屏录制光标位置检查：对同一静态 grid 画面的"光标开/关"两段成品逐像素差分，定位光标。

用法:
    python check_cursor.py 光标开.mp4 光标关.mp4 [--x0 1600] [--y0 0] [--cursor-x 2560] [--cursor-y 500] [--tol 24]

差分区域的包围盒中心与期望位置（桌面绝对坐标 - 选区左上角）比较；包围盒中心相对热点有偏移，故容差默认 24 px。
前提: 两段成品尺寸相同（建议 1920x1080 选区，不缩放）。顶部序号条每帧都变，差分时整体排除。
退出码: 0=通过，1=不通过，2=运行出错。
依赖: Python 3 标准库 + 系统 ffmpeg / ffprobe。
"""

import argparse
import subprocess
import sys
from typing import List, Optional, Sequence, Tuple

from check_seam import DEFAULT_FFMPEG_DIR, decode_gray_frames, find_tool, probe_size

# 灰度差分阈值（低于视为编码噪声）。
DIFF_THRESHOLD = 40
# 光标包围盒的合理最大边长（像素）；超过说明差分里混入了画面变化而非光标。
MAX_CURSOR_SIZE = 128
# 顶部序号条高度占画面高度的分母（与夹具 seqbar::BAR_HEIGHT_DIVISOR 一致）与最小高度。
BAR_DIVISOR = 16
BAR_MIN = 32
# 默认期望光标桌面坐标与选区左上角。
DEFAULT_CURSOR = (2560, 500)
DEFAULT_ORIGIN = (1600, 0)
# 默认热点偏移容差（像素）。
DEFAULT_TOL = 24.0

# 包围盒 (左, 上, 右, 下)，右下为闭区间。
BBox = Tuple[int, int, int, int]


def diff_bbox(on: bytes, off: bytes, width: int, height: int, skip_top: int, threshold: int = DIFF_THRESHOLD) -> Optional[BBox]:
    """求两帧灰度差异超阈值的像素包围盒（忽略顶部 skip_top 行）。

    参数:
        on、off: 两帧灰度（width*height 字节）。
        width、height: 帧尺寸。
        skip_top: 忽略的顶部行数。
        threshold: 差分阈值。
    返回:
        包围盒 (左, 上, 右, 下)；无差异返回 None。
    示例:
        >>> a = bytes(16); b = bytearray(16); b[5] = 200
        >>> diff_bbox(a, bytes(b), 4, 4, 0)
        (1, 1, 1, 1)
    """
    box: Optional[List[int]] = None
    for y in range(skip_top, height):
        base = y * width
        ra, rb = on[base:base + width], off[base:base + width]
        if ra == rb:
            continue
        for x in range(width):
            if abs(ra[x] - rb[x]) > threshold:
                if box is None:
                    box = [x, y, x, y]
                else:
                    box[0] = min(box[0], x)
                    box[2] = max(box[2], x)
                    box[3] = y
    return None if box is None else (box[0], box[1], box[2], box[3])


def bbox_center(box: BBox) -> Tuple[float, float]:
    """包围盒中心。

    参数:
        box: 包围盒。
    返回:
        (中心 x, 中心 y)。
    示例:
        >>> bbox_center((10, 20, 29, 39))
        (20.0, 30.0)
    """
    return (box[0] + box[2] + 1) / 2, (box[1] + box[3] + 1) / 2


def evaluate(box: Optional[BBox], expected: Tuple[float, float], tol: float) -> Tuple[bool, str]:
    """判定光标位置。

    参数:
        box: 差分包围盒（无差异为 None）。
        expected: 期望光标位置（相对选区左上角）。
        tol: 中心与期望位置的最大偏差（像素，x/y 各自）。
    返回:
        (是否通过, 说明)。
    示例:
        >>> evaluate((950, 495, 975, 525), (960, 500), 24)[0]
        True
        >>> evaluate(None, (960, 500), 24)[0]
        False
    """
    if box is None:
        return False, "光标开/关两段没有可见差异（光标未被录进画面）"
    w, h = box[2] - box[0] + 1, box[3] - box[1] + 1
    cx, cy = bbox_center(box)
    dx, dy = cx - expected[0], cy - expected[1]
    info = f"差分包围盒 {box}（{w}x{h}），中心=({cx:.1f},{cy:.1f}) 期望=({expected[0]:.0f},{expected[1]:.0f}) 偏差=({dx:+.1f},{dy:+.1f})"
    if w > MAX_CURSOR_SIZE or h > MAX_CURSOR_SIZE:
        return False, info + f"：包围盒超过 {MAX_CURSOR_SIZE}px，差分里混入了非光标变化"
    if abs(dx) > tol or abs(dy) > tol:
        return False, info + f"：超出容差 ±{tol:g}px"
    return True, info


def main(argv: Optional[List[str]] = None) -> int:
    """命令行入口。

    参数:
        argv: 参数列表（缺省取 sys.argv）。
    返回:
        0=通过，1=不通过，2=出错。
    """
    ap = argparse.ArgumentParser(description="跨屏录制光标位置检查")
    ap.add_argument("on", help="光标开的成品")
    ap.add_argument("off", help="光标关的成品")
    ap.add_argument("--x0", type=int, default=DEFAULT_ORIGIN[0], help="选区左边界（桌面 x）")
    ap.add_argument("--y0", type=int, default=DEFAULT_ORIGIN[1], help="选区上边界（桌面 y）")
    ap.add_argument("--cursor-x", type=int, default=DEFAULT_CURSOR[0], help="光标桌面 x")
    ap.add_argument("--cursor-y", type=int, default=DEFAULT_CURSOR[1], help="光标桌面 y")
    ap.add_argument("--tol", type=float, default=DEFAULT_TOL, help="热点偏移容差（像素）")
    ap.add_argument("--frames", type=int, default=4, help="抽取帧数（逐帧配对差分）")
    ap.add_argument("--ffmpeg-dir", default=DEFAULT_FFMPEG_DIR, help="ffmpeg/ffprobe 所在目录")
    args = ap.parse_args(argv)
    try:
        ffmpeg = find_tool("ffmpeg", args.ffmpeg_dir)
        ffprobe = find_tool("ffprobe", args.ffmpeg_dir)
        size_on, size_off = probe_size(ffprobe, args.on), probe_size(ffprobe, args.off)
        if size_on != size_off:
            print(f"出错: 两段成品尺寸不同 {size_on} vs {size_off}", file=sys.stderr)
            return 2
        width, height = size_on
        frames_on = decode_gray_frames(ffmpeg, args.on, width, height, args.frames)
        frames_off = decode_gray_frames(ffmpeg, args.off, width, height, args.frames)
    except (OSError, ValueError, subprocess.CalledProcessError) as e:
        print(f"出错: {e}", file=sys.stderr)
        return 2
    pairs = list(zip(frames_on, frames_off))
    if not pairs:
        print("出错: 没有解出任何帧", file=sys.stderr)
        return 2
    skip_top = max(height // BAR_DIVISOR, BAR_MIN) + 1
    expected = (args.cursor_x - args.x0, args.cursor_y - args.y0)
    results = [evaluate(diff_bbox(a, b, width, height, skip_top), expected, args.tol) for a, b in pairs]
    print(f"cursor-check: {width}x{height} 配对帧数={len(pairs)}")
    for i, (_, msg) in enumerate(results):
        print(f"  帧#{i}: {msg}")
    ok = all(r[0] for r in results)
    print("PASS" if ok else "FAIL")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
