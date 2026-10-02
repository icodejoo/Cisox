//! 系统引擎测试：与 FFmpeg 引擎对同一样片做一致性对比（容忍编解码差异）。
//!
//! 样片由 `testclip` 现生成，放系统临时目录。需要硬件 H.264 MFT 的测试在没有硬件时直接跳过。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use ffmpeg_next::format::Pixel;
use snow_recorder_protocol::{EditOp, EditRequest, EngineKind, ExtractMode, ImageFormat};

use super::reader::{MfSource, scan};
use super::{MfSession, SystemEngine, has_hardware_h264};
use crate::edit::source::VideoSource;
use crate::edit::testclip::{self, Clip};
use crate::edit::yuv::RgbConverter;
use crate::edit::{EditEngine, FfmpegEngine, OpSupport, TaskCtl, execute};

/// 静默的任务控制。
fn ctl(cancelled: bool) -> TaskCtl {
    TaskCtl::new(Arc::new(AtomicBool::new(cancelled)), Box::new(|_| {}))
}

/// 生成样片，返回（目录, 路径）。
fn make(name: &str, clip: &Clip) -> (PathBuf, PathBuf) {
    let dir = testclip::temp_dir(name);
    let path = dir.join("in.mp4");
    testclip::make_clip(&path, clip).unwrap();
    (dir, path)
}

/// 毫秒 -> 期望帧序号（取目标时刻正在显示的帧）。
fn expect_index(ms: u64, clip: &Clip) -> i32 {
    ((ms * u64::from(clip.fps)) / 1000).min(u64::from(clip.frames) - 1) as i32
}

/// 由灰度（BGR 首像素）反推帧序号。
fn index_of_gray(gray: u8) -> i32 {
    (f32::from(gray) * 219.0 / 255.0 / 3.0).round() as i32
}

/// 两个 BGR 缓冲的平均绝对差与最大差。
fn diff(a: &[u8], b: &[u8]) -> (f64, i32) {
    assert_eq!(a.len(), b.len());
    let (mut sum, mut max) = (0u64, 0i32);
    for (x, y) in a.iter().zip(b) {
        let d = (i32::from(*x) - i32::from(*y)).abs();
        sum += d as u64;
        max = max.max(d);
    }
    (sum as f64 / a.len() as f64, max)
}

/// 目录里没有残留的中间目录。
fn no_scratch(dir: &Path) -> bool {
    std::fs::read_dir(dir).unwrap().flatten().all(|e| {
        !e.file_name()
            .to_string_lossy()
            .starts_with(".snow-recording-")
    })
}

/// 探测：与 FFmpeg 引擎对同一样片的尺寸、帧数、关键帧数一致，时长和帧率在容差内。
#[test]
fn probe_matches_ffmpeg() {
    // 带音轨的样片曾让末样本时长读成 0、帧率算偏，所以两种都测
    for (bframes, audio) in [(0usize, false), (2, false), (0, true)] {
        let clip = Clip {
            bframes,
            audio,
            ..Clip::default()
        };
        let (dir, input) = make(&format!("sys-probe-{bframes}-{audio}"), &clip);
        let sys = SystemEngine.probe(&input).unwrap();
        let ff = FfmpegEngine.probe(&input).unwrap();
        assert_eq!((sys.width, sys.height), (ff.width, ff.height));
        assert_eq!(sys.frames, ff.frames, "bframes={bframes}");
        assert_eq!(sys.keyframes, ff.keyframes, "bframes={bframes}");
        assert!(
            sys.duration_ms.abs_diff(ff.duration_ms) <= 40,
            "时长 {} vs {}",
            sys.duration_ms,
            ff.duration_ms
        );
        assert!(
            sys.fps_milli.abs_diff(ff.fps_milli) <= 100,
            "fps {} vs {}",
            sys.fps_milli,
            ff.fps_milli
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// 精确 seek：GOP 起点/中间/末尾、帧边界前后，向前向后乱序，帧序号与 FFmpeg 引擎一致。
#[test]
fn seek_is_frame_accurate_and_matches_ffmpeg() {
    for bframes in [0usize, 2] {
        let clip = Clip {
            bframes,
            ..Clip::default()
        };
        let (dir, input) = make(&format!("sys-seek-{bframes}"), &clip);
        let _session = MfSession::start().unwrap();
        let mut sys = MfSource::open(&input, &scan(&input).unwrap()).unwrap();
        let mut ff = VideoSource::open(&input).unwrap();
        for ms in [
            0u64, 39, 40, 41, 959, 960, 961, 1500, 1919, 1920, 2000, 2879, 700, 20, 2800, 60_000,
        ] {
            let s = sys
                .frame_at_ms(ms)
                .unwrap_or_else(|e| panic!("ms={ms}: {e}"));
            let f = ff.frame_at_ms(ms).unwrap();
            let sys_index = index_of_gray(s.bgr[0]);
            let ff_index = testclip::frame_index_of(&f.frame);
            assert_eq!(ff_index, expect_index(ms, &clip), "FFmpeg 基线 ms={ms}");
            assert_eq!(sys_index, ff_index, "bframes={bframes} ms={ms}");
            assert!(
                s.ms() <= ms.min(2880) as i64,
                "返回帧晚于目标 {} > {ms}",
                s.ms()
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// 解码像素：有渐变的样片上，系统解码的整帧与 FFmpeg 解码转 BGR 的结果接近
/// （发现上下翻转、红蓝互换、色彩矩阵错误）。
#[test]
fn decoded_pixels_match_ffmpeg() {
    let clip = Clip {
        width: 128,
        height: 96,
        gradient: true,
        ..Clip::default()
    };
    let (dir, input) = make("sys-pixels", &clip);
    let _session = MfSession::start().unwrap();
    let mut sys = MfSource::open(&input, &scan(&input).unwrap()).unwrap();
    let mut ff = VideoSource::open(&input).unwrap();
    let mut conv = RgbConverter::new(Pixel::BGR24);
    for ms in [0u64, 1000, 2000] {
        let s = sys
            .frame_at_ms(ms)
            .unwrap_or_else(|e| panic!("ms={ms}: {e}"));
        let f = ff.frame_at_ms(ms).unwrap();
        let bgr = conv.convert(&f.frame).unwrap();
        let (w, h) = (bgr.width() as usize, bgr.height() as usize);
        assert_eq!((s.width as usize, s.height as usize), (w, h));
        let mut want = Vec::with_capacity(w * h * 3);
        for y in 0..h {
            want.extend_from_slice(&bgr.data(0)[y * bgr.stride(0)..y * bgr.stride(0) + w * 3]);
        }
        let (mean, max) = diff(&s.bgr, &want);
        assert!(
            mean <= 5.0 && max <= 24,
            "ms={ms} 平均差 {mean:.2} 最大差 {max}"
        );
        // 渐变必须真的存在，否则上面的对比测不出翻转；系统解码的上下方向也要对
        assert!(want[(h - 1) * w * 3] > want[0] + 10, "样片没有纵向渐变");
        assert!(s.bgr[(h - 1) * w * 3] > s.bgr[0] + 10, "系统解码上下翻转");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// 抽帧：PNG/JPEG 在三种模式下的张数、文件名与像素都和 FFmpeg 引擎一致。
#[test]
fn extract_matches_ffmpeg() {
    let clip = Clip {
        gradient: true,
        ..Clip::default()
    };
    let (dir, input) = make("sys-extract", &clip);
    let modes = [
        ExtractMode::Single { at_ms: 1000 },
        ExtractMode::Interval { every_ms: 400 },
        ExtractMode::Keyframes,
    ];
    for (mi, mode) in modes.into_iter().enumerate() {
        for format in [ImageFormat::Png, ImageFormat::Jpeg] {
            let run = |engine: EngineKind, tag: &str| {
                let out = dir.join(format!("{tag}-{mi}-{}", format.as_str()));
                let req = EditRequest {
                    engine,
                    op: EditOp::ExtractFrames {
                        mode,
                        format,
                        quality: 90,
                    },
                    input: input.clone(),
                    output: out.clone(),
                };
                let (report, used) = execute(&req, &ctl(false)).unwrap();
                assert_eq!(used, engine);
                (out, report.frames)
            };
            let (sys_dir, sys_n) = run(EngineKind::System, "sys");
            let (ff_dir, ff_n) = run(EngineKind::Ffmpeg, "ff");
            assert_eq!(sys_n, ff_n, "{mode:?} {format:?}");
            let names = |d: &Path| {
                let mut v: Vec<String> = std::fs::read_dir(d)
                    .unwrap()
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect();
                v.sort();
                v
            };
            let (a, b) = (names(&sys_dir), names(&ff_dir));
            assert_eq!(a, b, "文件名（含帧时间）应一致 {mode:?} {format:?}");
            for name in &a {
                let (w1, h1, p1) = testclip::decode_bgr(&sys_dir.join(name));
                let (w2, h2, p2) = testclip::decode_bgr(&ff_dir.join(name));
                assert_eq!((w1, h1), (w2, h2));
                let (mean, max) = diff(&p1, &p2);
                assert!(
                    mean <= 5.0 && max <= 24,
                    "{name} {format:?} 平均差 {mean:.2} 最大差 {max}"
                );
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// 无损 WebP：系统引擎给出可回落的"不支持"，不留输出。
#[test]
fn webp_is_unsupported_and_falls_back() {
    let (dir, input) = make("sys-webp", &Clip::default());
    let req = EditRequest {
        engine: EngineKind::System,
        op: EditOp::ExtractFrames {
            mode: ExtractMode::Single { at_ms: 0 },
            format: ImageFormat::WebpLossless,
            quality: 90,
        },
        input: input.clone(),
        output: dir.join("out"),
    };
    let err = execute(&req, &ctl(false)).unwrap_err();
    assert!(err.to_string().contains("WebP"), "{err}");
    let direct = SystemEngine.run(&req, &ctl(false)).unwrap_err();
    assert!(direct.can_fallback());
    assert!(!dir.join("out").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

/// 取消：返回取消错误，输出目录与中间目录都不存在，取消不触发回落。
#[test]
fn extract_cancel_leaves_nothing() {
    let (dir, input) = make("sys-cancel", &Clip::default());
    let req = EditRequest {
        engine: EngineKind::Auto,
        op: EditOp::ExtractFrames {
            mode: ExtractMode::Interval { every_ms: 100 },
            format: ImageFormat::Png,
            quality: 90,
        },
        input,
        output: dir.join("out"),
    };
    let err = execute(&req, &ctl(true)).unwrap_err();
    assert!(err.is_cancelled() && !err.can_fallback());
    assert!(!dir.join("out").exists());
    assert!(no_scratch(&dir));
    let _ = std::fs::remove_dir_all(&dir);
}

/// 损坏输入：探测返回可读错误，且允许回落（让 FFmpeg 给出它的错误）。
#[test]
fn corrupt_input_is_fallback_error() {
    let dir = testclip::temp_dir("sys-corrupt");
    let input = dir.join("bad.mp4");
    std::fs::write(&input, b"this is not a video").unwrap();
    let err = SystemEngine.probe(&input).unwrap_err();
    assert!(!err.to_string().is_empty() && err.can_fallback());
    let _ = std::fs::remove_dir_all(&dir);
}

/// 扫描出的关键帧表正确。
#[test]
fn scan_keyframes_match() {
    let (dir, input) = make("sys-keys", &Clip::default());
    let _session = MfSession::start().unwrap();
    let scanned = scan(&input).unwrap();
    let (info, keys) = (scanned.info, scanned.keys);
    let ms: Vec<i64> = keys.iter().map(|k| super::reader::hns_to_ms(*k)).collect();
    // 72 帧、GOP 24、25fps：关键帧在 0 / 960 / 1920 ms
    assert_eq!(ms, vec![0, 960, 1920]);
    assert_eq!(info.keyframes, 3);
    // 带 B 帧时 MP4 首帧显示时间后移，起点被扣掉后关键帧时间不变
    let (dir2, input2) = make(
        "sys-keys-b",
        &Clip {
            bframes: 2,
            ..Clip::default()
        },
    );
    let b = scan(&input2).unwrap();
    assert_eq!(
        b.keys
            .iter()
            .map(|k| super::reader::hns_to_ms(*k))
            .collect::<Vec<_>>(),
        ms
    );
    let _ = std::fs::remove_dir_all(&dir2);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 需要硬件 H.264 的测试用的样片（编码器对极小分辨率有下限，用 320x240）。
fn hw_clip() -> Clip {
    Clip {
        width: 320,
        height: 240,
        audio: true,
        ..Clip::default()
    }
}

/// 探测输出文件（用 FFmpeg 引擎，独立于被测引擎）。
fn probe_out(path: &Path) -> snow_recorder_protocol::ProbeInfo {
    FfmpegEngine.probe(path).unwrap()
}

/// 降 fps：帧数与 FFmpeg 引擎同一网格、分辨率不变、音频包原样保留、首帧内容正确；无硬件则跳过。
#[test]
fn reduce_fps_matches_ffmpeg() {
    if !has_hardware_h264() {
        eprintln!("没有硬件 H.264 MFT，跳过");
        return;
    }
    let clip = hw_clip();
    let (dir, input) = make("sys-fps", &clip);
    let op = EditOp::ReduceFps { target_fps: 10 };
    let run = |engine, name: &str| {
        let req = EditRequest {
            engine,
            op,
            input: input.clone(),
            output: dir.join(name),
        };
        execute(&req, &ctl(false)).unwrap()
    };
    let (sys_report, used) = run(EngineKind::System, "sys.mp4");
    assert_eq!(used, EngineKind::System);
    let (ff_report, _) = run(EngineKind::Ffmpeg, "ff.mp4");
    assert_eq!(
        sys_report.frames, ff_report.frames,
        "同一 PTS 网格应选出同样张数"
    );
    let (sys, ff) = (
        probe_out(&dir.join("sys.mp4")),
        probe_out(&dir.join("ff.mp4")),
    );
    assert_eq!(sys.frames, ff.frames);
    assert_eq!((sys.width, sys.height), (clip.width, clip.height));
    assert!(
        sys.duration_ms.abs_diff(ff.duration_ms) <= 150,
        "{sys:?} {ff:?}"
    );
    assert!(
        (9_000..=11_000).contains(&sys.fps_milli),
        "fps {}",
        sys.fps_milli
    );
    let a_in = testclip::audio_facts(&input).unwrap();
    let a_out = testclip::audio_facts(&dir.join("sys.mp4")).expect("系统引擎输出应保留音频");
    assert_eq!(a_in.packets, a_out.packets);
    assert_eq!(a_in.rate, a_out.rate);
    let mut out = VideoSource::open(&dir.join("sys.mp4")).unwrap();
    assert_eq!(
        testclip::frame_index_of(&out.frame_at_ms(0).unwrap().frame),
        0
    );
    assert!(no_scratch(&dir));
    let _ = std::fs::remove_dir_all(&dir);
}

/// 缩放：尺寸、帧数、时长与 FFmpeg 引擎一致，音频保留，内容仍是同一时刻的灰阶；无硬件则跳过。
#[test]
fn scale_matches_ffmpeg() {
    if !has_hardware_h264() {
        eprintln!("没有硬件 H.264 MFT，跳过");
        return;
    }
    let clip = hw_clip();
    let (dir, input) = make("sys-scale", &clip);
    let op = EditOp::Scale {
        width: 160,
        height: 120,
    };
    for (engine, name) in [
        (EngineKind::System, "sys.mp4"),
        (EngineKind::Ffmpeg, "ff.mp4"),
    ] {
        let req = EditRequest {
            engine,
            op,
            input: input.clone(),
            output: dir.join(name),
        };
        let (report, _) = execute(&req, &ctl(false)).unwrap();
        eprintln!("{name}: 报告帧数 {}", report.frames);
    }
    let (sys, ff) = (
        probe_out(&dir.join("sys.mp4")),
        probe_out(&dir.join("ff.mp4")),
    );
    assert_eq!((sys.width, sys.height), (160, 120));
    assert_eq!(sys.frames, ff.frames, "{sys:?} {ff:?}");
    assert!(
        sys.duration_ms.abs_diff(ff.duration_ms) <= 80,
        "{sys:?} {ff:?}"
    );
    assert!(testclip::audio_facts(&dir.join("sys.mp4")).is_some());
    let mut out = VideoSource::open(&dir.join("sys.mp4")).unwrap();
    let idx = testclip::frame_index_of(&out.frame_at_ms(1000).unwrap().frame);
    assert!((23..=27).contains(&idx), "帧序号 {idx}");
    assert!(no_scratch(&dir));
    let _ = std::fs::remove_dir_all(&dir);
}

/// 转码取消：返回取消错误，不留输出与中间目录，且不回落；无硬件则跳过。
#[test]
fn transcode_cancel_leaves_nothing() {
    if !has_hardware_h264() {
        return;
    }
    let (dir, input) = make("sys-tc-cancel", &hw_clip());
    let req = EditRequest {
        engine: EngineKind::Auto,
        op: EditOp::Scale {
            width: 160,
            height: 120,
        },
        input,
        output: dir.join("o.mp4"),
    };
    let err = execute(&req, &ctl(true)).unwrap_err();
    assert!(err.is_cancelled());
    assert!(!dir.join("o.mp4").exists() && no_scratch(&dir));
    let _ = std::fs::remove_dir_all(&dir);
}

/// 能力：没有硬件编码器时，重编码类操作标不可用并给出原因。
#[test]
fn caps_reflect_hardware() {
    let info = snow_recorder_protocol::ProbeInfo {
        width: 1,
        height: 1,
        duration_ms: 1,
        fps_milli: 1,
        frames: 1,
        keyframes: 1,
    };
    let caps = SystemEngine.capabilities(&info);
    let want = if has_hardware_h264() {
        OpSupport::Available
    } else {
        OpSupport::Unavailable("系统没有硬件 H.264 编码 MFT".to_string())
    };
    assert_eq!(caps.reduce_fps, want);
    assert_eq!(caps.scale, want);
}

/// 1080p 样片：解码器常把高度对齐到 1088，裁剪后的尺寸和像素必须与 FFmpeg 引擎一致
/// （抽帧总是测；重编码部分需要硬件 H.264，无则跳过）。
#[test]
fn hd_1080p_matches_ffmpeg() {
    let clip = Clip {
        width: 1920,
        height: 1080,
        frames: 30,
        fps: 30,
        gop: 30,
        gradient: true,
        ..Clip::default()
    };
    let (dir, input) = make("sys-hd", &clip);
    let run = |engine: EngineKind, op: EditOp, name: &str| {
        let req = EditRequest {
            engine,
            op,
            input: input.clone(),
            output: dir.join(name),
        };
        execute(&req, &ctl(false)).unwrap();
        dir.join(name)
    };
    let png = EditOp::ExtractFrames {
        mode: ExtractMode::Single { at_ms: 500 },
        format: ImageFormat::Png,
        quality: 90,
    };
    let find = |d: &Path| {
        std::fs::read_dir(d)
            .unwrap()
            .flatten()
            .next()
            .unwrap()
            .path()
    };
    let sys = find(&run(EngineKind::System, png, "sys-png"));
    let ff = find(&run(EngineKind::Ffmpeg, png, "ff-png"));
    let (a, b) = (testclip::decode_bgr(&sys), testclip::decode_bgr(&ff));
    assert_eq!((a.0, a.1), (1920, 1080));
    assert_eq!((a.0, a.1), (b.0, b.1));
    let (mean, max) = diff(&a.2, &b.2);
    assert!(
        mean <= 5.0 && max <= 24,
        "抽帧 平均差 {mean:.2} 最大差 {max}"
    );
    if !has_hardware_h264() {
        eprintln!("没有硬件 H.264 MFT，跳过重编码部分");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }
    let scale = EditOp::Scale {
        width: 1280,
        height: 720,
    };
    let fps = EditOp::ReduceFps { target_fps: 15 };
    let mut problems: Vec<String> = Vec::new();
    for (op, name, dims) in [(scale, "scale", (1280, 720)), (fps, "fps", (1920, 1080))] {
        let sys = run(EngineKind::System, op, &format!("sys-{name}.mp4"));
        let ff = run(EngineKind::Ffmpeg, op, &format!("ff-{name}.mp4"));
        let (si, fi) = (probe_out(&sys), probe_out(&ff));
        assert_eq!((si.width, si.height), dims, "{name}");
        assert_eq!(si.frames, fi.frames, "{name} 帧数 {si:?} {fi:?}");
        let frame = |p: &Path| {
            let mut v = VideoSource::open(p).unwrap();
            let d = v.frame_at_ms(500).unwrap();
            let mut conv = RgbConverter::new(Pixel::BGR24);
            let bgr = conv.convert(&d.frame).unwrap();
            let (w, h) = (bgr.width() as usize, bgr.height() as usize);
            let mut out = Vec::with_capacity(w * h * 3);
            for y in 0..h {
                out.extend_from_slice(&bgr.data(0)[y * bgr.stride(0)..y * bgr.stride(0) + w * 3]);
            }
            out
        };
        let (fs, ffr) = (frame(&sys), frame(&ff));
        let (mean, max) = diff(&fs, &ffr);
        let w = dims.0 as usize;
        let rows: Vec<usize> = (0..fs.len() / (w * 3))
            .filter(|y| {
                fs[y * w * 3..(y + 1) * w * 3]
                    .iter()
                    .zip(&ffr[y * w * 3..(y + 1) * w * 3])
                    .map(|(a, b)| (i32::from(*a) - i32::from(*b)).abs())
                    .max()
                    .unwrap_or(0)
                    > 60
            })
            .collect();
        if !(mean <= 8.0 && max <= 60) {
            problems.push(format!(
                "{name} 坏行 {rows:?} 平均差 {mean:.2} 最大差 {max}"
            ));
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
    let _ = std::fs::remove_dir_all(&dir);
}
