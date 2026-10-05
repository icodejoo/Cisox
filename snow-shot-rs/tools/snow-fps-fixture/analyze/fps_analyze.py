#!/usr/bin/env python3
"""录制成品帧率分析：从画面顶部序号色块条读出每帧序号，统计帧率、丢帧、重复与漂移。

用法:
    python fps_analyze.py 成品.mp4 --refresh 59 [--log 夹具帧日志.csv] [--json]

判定线（用户已定）: 有效帧率 >= 刷新率 * 0.95 且 丢帧率 < 1%。
依赖: Python 3 标准库 + 系统 ffmpeg / ffprobe（PATH 或 --ffmpeg-dir 指定）。
"""

import argparse
import csv
import json
import shutil
import statistics
import subprocess
import sys
from dataclasses import asdict, dataclass, field
from typing import List, Optional, Sequence, Tuple

# 序号位数（与夹具 seqbar::SEQ_BITS 一致）。
SEQ_BITS = 32
# 解码阈值（灰度 0..255 中点）。
DECODE_THRESHOLD = 128
# 达标比例：有效帧率 / 刷新率。
FPS_RATIO_LINE = 0.95
# 丢帧率上限。
DROP_RATE_LIMIT = 0.01
# 至少这么多个有效帧才把启动首帧排除出稳态丢帧口径。
STEADY_MIN_FRAMES = 30
# 不带夹具日志时，序号偏离中位数超过该值视为解码错误。
OUTLIER_SEQ_SPAN = 1_000_000
# 抽取序号条中间一半高度的 ffmpeg 滤镜（对缩放后的成品同样适用）。
BAR_FILTER = f"crop=iw:ih/32:0:ih/64,scale={SEQ_BITS}:1:flags=area,format=gray"


def bar_filter(bar_crop: Optional[Tuple[int, int, int]] = None) -> str:
    """构造抽序号条的 ffmpeg 滤镜；双窗口时只裁主窗口那段（按比例，缩放后的成品同样适用）。

    参数:
        bar_crop: (x 偏移, 宽, 区域总宽)，即夹具 --check 输出的 bar_crop；None 表示整宽。
    返回:
        滤镜字符串。
    示例:
        >>> bar_filter((0, 1280, 2560)).startswith("crop=iw*1280/2560:")
        True
    """
    if bar_crop is None:
        return BAR_FILTER
    x, w, total = bar_crop
    return f"crop=iw*{w}/{total}:ih/32:iw*{x}/{total}:ih/64,scale={SEQ_BITS}:1:flags=area,format=gray"


def parse_bar_crop(text: str) -> Tuple[int, int, int]:
    """解析 "x,w,total"。

    参数:
        text: 逗号分隔的三个整数（w、total 须大于 0 且 x+w 不超过 total）。
    返回:
        (x, w, total)。
    示例:
        >>> parse_bar_crop("0,1280,2560")
        (0, 1280, 2560)
    """
    x, w, total = (int(v) for v in text.split(","))
    if x < 0 or w <= 0 or total <= 0 or x + w > total:
        raise ValueError(f"bar_crop 非法: {text}")
    return x, w, total


def decode_seq(samples: Sequence[int]) -> Optional[int]:
    """把 32 个灰度采样（高位在前）还原成序号；不足 32 个返回 None。

    参数:
        samples: 灰度值序列，取前 32 个。
    返回:
        序号，或 None。
    示例:
        >>> decode_seq([0] * 31 + [255])
        1
    """
    if len(samples) < SEQ_BITS:
        return None
    value = 0
    for v in samples[:SEQ_BITS]:
        value = (value << 1) | (1 if v >= DECODE_THRESHOLD else 0)
    return value


def decode_raw_frames(raw: bytes) -> List[Optional[int]]:
    """把 ffmpeg 输出的原始灰度流（每帧 32 字节）拆成序号列表。

    参数:
        raw: 原始字节；尾部不足一帧的残余被忽略。
    返回:
        每帧一个序号。
    示例:
        >>> decode_raw_frames(bytes([0] * 31 + [255]) * 2)
        [1, 1]
    """
    n = len(raw) // SEQ_BITS
    return [decode_seq(raw[i * SEQ_BITS:(i + 1) * SEQ_BITS]) for i in range(n)]


@dataclass
class Stats:
    """分析结果。"""

    frames: int = 0  # 成品帧数
    decoded: int = 0  # 序号有效的帧数
    bad_frames: int = 0  # 序号无效/离群的帧数
    container_duration_s: float = 0.0  # 容器给出的时长
    pts_span_s: float = 0.0  # 首末帧 pts 之差
    duration_s: float = 0.0  # 用于计算帧率的实际时长
    unique_seqs: int = 0  # 不同序号数
    fps_effective: float = 0.0  # 有效帧率 = 不同序号数 / 时长
    seq_min: int = 0
    seq_max: int = 0
    dropped_seqs: int = 0  # 稳态序号区间内缺失的序号数（已排除启动首帧）
    drop_rate: float = 0.0  # 稳态缺失 / 稳态区间长度，判定用
    dropped_seqs_raw: int = 0  # 含启动首帧的缺失序号数（原始口径）
    drop_rate_raw: float = 0.0  # 含启动首帧的丢帧比例（原始口径）
    startup_skipped: bool = False  # 是否已把启动首帧排除出稳态口径
    duplicate_frames: int = 0  # 序号重复的帧数
    out_of_order: int = 0  # 序号倒退次数
    max_seq_gap: int = 0  # 相邻帧最大序号跳变
    published_fps: Optional[float] = None  # 夹具日志给出的出帧率
    drift_ms: Optional[float] = None  # 序号→时间戳线性漂移（整段累计，毫秒）
    max_abs_dev_ms: Optional[float] = None  # 去掉常量偏移后的最大偏差（毫秒）
    refresh_hz: float = 0.0  # 判定用刷新率
    fps_line: float = 0.0  # 帧率达标线
    fps_ok: bool = False
    drop_ok: bool = False
    order_ok: bool = False
    verdict: str = ""
    notes: List[str] = field(default_factory=list)


def linear_fit(xs: Sequence[float], ys: Sequence[float]) -> Tuple[float, float]:
    """最小二乘直线拟合 y = a*x + b。

    参数:
        xs, ys: 等长序列，至少 2 个点。
    返回:
        (斜率 a, 截距 b)；退化（x 全相同）时斜率为 0。
    示例:
        >>> linear_fit([0, 1, 2], [1, 3, 5])
        (2.0, 1.0)
    """
    n = len(xs)
    mx = sum(xs) / n
    my = sum(ys) / n
    sxx = sum((x - mx) ** 2 for x in xs)
    if sxx == 0:
        return 0.0, my
    a = sum((x - mx) * (y - my) for x, y in zip(xs, ys)) / sxx
    return a, my - a * mx


def compute_stats(
    frames: Sequence[Tuple[float, Optional[int]]],
    container_duration_s: float,
    refresh_hz: float,
    submit_s_by_seq: Optional[dict] = None,
) -> Stats:
    """由 (pts 秒, 序号) 列表计算全部指标与判定。

    参数:
        frames: 每帧 (pts_time, 序号或 None)，按显示顺序。
        container_duration_s: 容器时长；<=0 时改用 pts 跨度 + 一个中位间隔。
        refresh_hz: 判定用刷新率（Hz），达标线 = 刷新率 * 0.95。
        submit_s_by_seq: 可选，夹具日志 {序号: 相对提交时间(秒)}，用于漂移和过滤离群序号。
    返回:
        Stats。
    示例:
        >>> fr = [(i / 60, i + 1) for i in range(60)]
        >>> s = compute_stats(fr, 1.0, 59.0)
        >>> (s.dropped_seqs, s.verdict)
        (0, 'PASS')
    """
    st = Stats(frames=len(frames), refresh_hz=refresh_hz, fps_line=refresh_hz * FPS_RATIO_LINE)
    good: List[Tuple[float, int]] = []
    valid_seqs = [s for _, s in frames if s is not None]
    center = statistics.median(valid_seqs) if valid_seqs else 0
    for pts, seq in frames:
        if seq is None:
            continue
        if submit_s_by_seq is not None:
            if seq not in submit_s_by_seq:
                continue
        elif abs(seq - center) > OUTLIER_SEQ_SPAN:
            continue
        good.append((pts, seq))
    st.decoded = len(good)
    st.bad_frames = st.frames - st.decoded
    if not good:
        st.verdict = "FAIL (无可解码帧)"
        return st

    pts_list = [p for p, _ in good]
    seqs = [s for _, s in good]
    st.pts_span_s = max(pts_list) - min(pts_list)
    if container_duration_s > 0:
        st.container_duration_s = container_duration_s
        st.duration_s = container_duration_s
    else:
        gaps = [b - a for a, b in zip(pts_list, pts_list[1:]) if b > a]
        st.duration_s = st.pts_span_s + (statistics.median(gaps) if gaps else 0.0)
    uniq = set(seqs)
    st.unique_seqs = len(uniq)
    st.seq_min, st.seq_max = min(seqs), max(seqs)
    span = st.seq_max - st.seq_min + 1
    st.dropped_seqs_raw = span - st.unique_seqs
    st.drop_rate_raw = st.dropped_seqs_raw / span if span else 0.0
    # 稳态口径：成品第 1 帧是复制接口刚建立时取到的陈旧启动帧，它与随后第一个新帧之间被合并的一帧
    # 发生在录制开始之前，不算录制中的丢帧；帧数太少时不排除。
    steady = seqs[1:] if len(seqs) >= STEADY_MIN_FRAMES else seqs
    st.startup_skipped = steady is not seqs
    steady_span = max(steady) - min(steady) + 1
    st.dropped_seqs = steady_span - len(set(steady))
    st.drop_rate = st.dropped_seqs / steady_span if steady_span else 0.0
    st.duplicate_frames = len(seqs) - st.unique_seqs
    st.out_of_order = sum(1 for a, b in zip(seqs, seqs[1:]) if b < a)
    st.max_seq_gap = max((b - a for a, b in zip(seqs, seqs[1:])), default=0)
    st.fps_effective = st.unique_seqs / st.duration_s if st.duration_s > 0 else 0.0

    if submit_s_by_seq is not None:
        xs, ys = [], []
        for pts, seq in good:
            if seq in submit_s_by_seq:
                xs.append(submit_s_by_seq[seq])
                ys.append(pts)
        if len(xs) >= 2:
            offset = statistics.median(y - x for x, y in zip(xs, ys))
            st.max_abs_dev_ms = max(abs(y - x - offset) for x, y in zip(xs, ys)) * 1000
            slope, _ = linear_fit(xs, ys)
            st.drift_ms = (slope - 1.0) * (max(xs) - min(xs)) * 1000
        times = [submit_s_by_seq[s] for s in (st.seq_min, st.seq_max) if s in submit_s_by_seq]
        if len(times) == 2 and times[1] > times[0]:
            st.published_fps = (st.seq_max - st.seq_min) / (times[1] - times[0])

    st.fps_ok = st.fps_effective >= st.fps_line
    st.drop_ok = st.drop_rate < DROP_RATE_LIMIT
    st.order_ok = st.out_of_order == 0
    parts = []
    if not st.fps_ok:
        parts.append(f"帧率 {st.fps_effective:.2f} < 达标线 {st.fps_line:.2f}")
    if not st.drop_ok:
        parts.append(f"丢帧率 {st.drop_rate * 100:.2f}% >= 1%")
    if not st.order_ok:
        parts.append(f"序号乱序 {st.out_of_order} 次")
    st.verdict = "PASS" if not parts else "FAIL (" + "; ".join(parts) + ")"
    return st


def load_fixture_log(path: str) -> dict:
    """读取夹具帧日志 CSV，返回 {序号: 相对提交时间(秒)}。

    参数:
        path: CSV 路径（表头 seq,submit_ns,flush_ns,unix_us）。
    返回:
        字典；无法解析的行被跳过。
    """
    out = {}
    with open(path, newline="", encoding="utf-8") as fh:
        for row in csv.DictReader(fh):
            try:
                out[int(row["seq"])] = int(row["submit_ns"]) / 1e9
            except (KeyError, ValueError):
                continue
    return out


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


def probe_frames(ffprobe: str, path: str) -> Tuple[List[float], float]:
    """用 ffprobe 读取每帧 pts 与容器时长。

    参数:
        ffprobe: ffprobe 路径。
        path: 成品路径。
    返回:
        (每帧 pts 秒列表, 容器时长秒；无则 0)。
    """
    cmd = [ffprobe, "-v", "error", "-select_streams", "v:0", "-show_entries", "frame=pts_time,best_effort_timestamp_time", "-of", "csv=p=0", path]
    out = subprocess.run(cmd, capture_output=True, text=True, check=True).stdout
    pts = []
    for line in out.splitlines():
        for cell in line.split(","):
            try:
                pts.append(float(cell))
                break
            except ValueError:
                continue
    dur_out = subprocess.run([ffprobe, "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", path], capture_output=True, text=True, check=True).stdout.strip()
    try:
        dur = float(dur_out)
    except ValueError:
        dur = 0.0
    return pts, dur


def decode_video(ffmpeg: str, path: str, bar_crop: Optional[Tuple[int, int, int]] = None) -> List[Optional[int]]:
    """用 ffmpeg 抽出每帧序号条并解码。

    参数:
        ffmpeg: ffmpeg 路径。
        path: 成品路径。
        bar_crop: 双窗口时序号条所在段，见 bar_filter。
    返回:
        每帧序号（解码失败为 None）。
    """
    cmd = [ffmpeg, "-v", "error", "-i", path, "-vf", bar_filter(bar_crop), "-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", "gray", "-"]
    raw = subprocess.run(cmd, capture_output=True, check=True).stdout
    return decode_raw_frames(raw)


def format_report(st: Stats) -> str:
    """把 Stats 排成多行文本。

    参数:
        st: 分析结果。
    """
    lines = [
        f"成品帧数            : {st.frames}（序号有效 {st.decoded}，无效 {st.bad_frames}）",
        f"容器时长 / pts 跨度 : {st.container_duration_s:.3f}s / {st.pts_span_s:.3f}s",
        f"有效帧率            : {st.fps_effective:.2f} fps（不同序号 {st.unique_seqs} / {st.duration_s:.3f}s）",
        f"序号区间            : {st.seq_min} .. {st.seq_max}",
        f"被丢序号数          : {st.dropped_seqs}（丢帧率 {st.drop_rate * 100:.3f}%，最大序号跳变 {st.max_seq_gap}）",
        f"启动首帧边界        : {'已排除出上面的稳态口径' if st.startup_skipped else '帧数太少，未排除'}；含首帧的原始口径 缺 {st.dropped_seqs_raw} 个（{st.drop_rate_raw * 100:.3f}%）",
        f"重复帧数            : {st.duplicate_frames}",
        f"序号乱序次数        : {st.out_of_order}",
    ]
    if st.published_fps is not None:
        lines.append(f"夹具出帧率          : {st.published_fps:.2f} fps")
    if st.drift_ms is not None:
        lines.append(f"序号→时间戳漂移     : {st.drift_ms:+.2f} ms（去偏移后最大偏差 {st.max_abs_dev_ms:.2f} ms）")
    lines.append(f"判定线              : 帧率 >= {st.fps_line:.2f} fps (刷新率 {st.refresh_hz:g}Hz x 95%)，丢帧率 < 1%")
    lines.append(f"判定                : {st.verdict}")
    return "\n".join(lines)


def main(argv: Optional[Sequence[str]] = None) -> int:
    """命令行入口；PASS 返回 0，FAIL 返回 1，工具/输入错误返回 2。

    参数:
        argv: 参数列表，缺省取 sys.argv。
    """
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("video")
    ap.add_argument("--refresh", type=float, default=59.0, help="判定用刷新率 Hz（默认 59）")
    ap.add_argument("--log", help="夹具帧日志 CSV（可选，用于漂移和序号校验）")
    ap.add_argument("--ffmpeg-dir", help="ffmpeg/ffprobe 所在目录")
    ap.add_argument("--json", action="store_true", help="输出 JSON")
    ap.add_argument("--bar-crop", help="双窗口时序号条所在段 x,w,total（取自夹具 --check 的 bar_crop）")
    args = ap.parse_args(argv)
    try:
        ffmpeg = find_tool("ffmpeg", args.ffmpeg_dir)
        ffprobe = find_tool("ffprobe", args.ffmpeg_dir)
        pts, dur = probe_frames(ffprobe, args.video)
        seqs = decode_video(ffmpeg, args.video, parse_bar_crop(args.bar_crop) if args.bar_crop else None)
        log = load_fixture_log(args.log) if args.log else None
    except (OSError, ValueError, subprocess.CalledProcessError) as e:
        print(f"错误: {e}", file=sys.stderr)
        return 2
    n = min(len(pts), len(seqs))
    if len(pts) != len(seqs):
        print(f"警告: ffprobe 帧数 {len(pts)} 与解码帧数 {len(seqs)} 不一致，按较小值对齐", file=sys.stderr)
    st = compute_stats(list(zip(pts[:n], seqs[:n])), dur, args.refresh, log)
    print(json.dumps(asdict(st), ensure_ascii=False, indent=2) if args.json else format_report(st))
    return 0 if st.verdict == "PASS" else 1


if __name__ == "__main__":
    sys.exit(main())
