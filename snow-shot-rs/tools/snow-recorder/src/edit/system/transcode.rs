//! 系统引擎重编码：降 fps 与缩放（共用一条流水线），音频包直通。
//!
//! SourceReader 解码成 NV12（缩放由其内置视频处理器完成），按 PTS 网格挑帧并改写时间戳，
//! 样本原样交给带硬件 H.264 MFT 的 SinkWriter，由它直接封装 MP4。
//! 码率沿用录制侧的"每像素每帧比特数"估算；拒绝静默使用微软软件 MFT。
//! 先写进中间文件（`.snow-recording-<pid>` 规则），成功后原子改名。

use std::path::Path;

use ffmpeg_next::Rational;
use snow_recorder_protocol::{EditOp, ProbeInfo, scratch_dir, scratch_file};
use windows::Win32::Foundation::RECT;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::core::{GUID, Interface, PCWSTR};

use super::reader::{
    Processing, ReadOut, Streams, find_streams, open_reader, read_sample, scan, select_stream,
};
use super::{MfSession, mf_err, mf_init_err};
use crate::edit::transcode::{FpsGrid, Target, plan_target};
use crate::edit::{EditError, TaskCtl};
use crate::win::mfenc::{configured_bpp, pack_pair, target_bitrate, verify_hardware_transform};

/// 进度阶段名。
const STAGE_TRANSCODE: &str = "transcode";
/// 每秒的 hns 数。
const HNS_PER_SECOND: i64 = 10_000_000;
/// 关键帧间隔（秒）。
const GOP_SECONDS: u32 = 2;
/// 样本时间被系统截断到整 hns，恰好落在网格中点的帧（如 30 到 15fps 的奇数帧）会被判到前一个槽位；
/// 补 1 hns 抵消截断，让选帧结果与 FFmpeg 引擎（精确时间基）一致。
const HNS_ROUNDING: i64 = 1;
/// hns 时间基（给 `FpsGrid` 用）。
const HNS_TIME_BASE: Rational = Rational(1, 10_000_000);

/// 执行降 fps / 缩放。
///
/// # 参数
/// - `input`：输入视频。
/// - `output`：输出 mp4 路径。
/// - `op`：`ReduceFps` 或 `Scale`。
/// - `ctl`：进度与取消控制。
///
/// # 返回
/// 写出的视频帧数；取消返回 `cancelled`。失败或取消时不留下中间文件。
pub fn run(input: &Path, output: &Path, op: EditOp, ctl: &TaskCtl) -> Result<u64, EditError> {
    let pid = std::process::id();
    let dir = scratch_dir(output, pid);
    std::fs::create_dir_all(&dir).map_err(|e| EditError::new(format!("创建中间目录失败: {e}")))?;
    let tmp = scratch_file(output, pid);
    let result = transcode_to(input, &tmp, op, ctl).and_then(|n| {
        std::fs::rename(&tmp, output)
            .map_err(|e| EditError::new(format!("移动输出文件失败: {e}")))?;
        Ok(n)
    });
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// 给媒体类型补齐色彩标注（缺失的按限幅 BT.709 填，与录制侧一致）。
fn ensure_color(media: &IMFMediaType) -> Result<(), EditError> {
    // SAFETY: 局部有效值。
    unsafe {
        let set = |key: &GUID, value: u32| -> Result<(), EditError> {
            if media.GetUINT32(key).is_err() {
                media
                    .SetUINT32(key, value)
                    .map_err(|e| mf_init_err("设置色彩标注失败", e))?;
            }
            Ok(())
        };
        set(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
        set(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
        set(&MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0 as u32)?;
        set(&MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_709.0 as u32)
    }
}

/// 创建 SinkWriter（硬件变换开；节流保持默认开启，离线转码要靠它限制输入积压的内存）。
fn open_writer(path: &Path) -> Result<IMFSinkWriter, EditError> {
    let wide: Vec<u16> = path
        .as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .chain(Some(0))
        .collect();
    // SAFETY: 局部有效值；wide 以 0 结尾，调用期间存活。
    unsafe {
        let mut attrs = None;
        MFCreateAttributes(&mut attrs, 1).map_err(|e| mf_init_err("创建属性失败", e))?;
        let attrs: IMFAttributes = attrs.ok_or_else(|| EditError::unsupported("属性为空"))?;
        attrs
            .SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)
            .map_err(|e| mf_init_err("开启硬件变换失败", e))?;
        MFCreateSinkWriterFromURL(PCWSTR(wide.as_ptr()), None, &attrs)
            .map_err(|e| mf_init_err("创建 SinkWriter 失败", e))
    }
}

/// 视频处理器 MFT 缩放器（NV12 到 NV12，一进一出，保留样本时间戳）。
struct Resizer {
    /// 视频处理器 MFT。
    mft: IMFTransform,
    /// 输出样本缓冲字节数。
    out_bytes: u32,
}

/// 创建缩放器。
///
/// # 参数
/// - `src`：输入（解码输出）类型。
/// - `buffer`：源缓冲的实际宽高（高度常对齐到 16，比显示尺寸大）。
/// - `area`：源画面里要保留的区域大小（左上角起）。
/// - `size`：目标宽高。
/// - `rate`：源帧率（分子, 分母），必须是源的而不是目标帧率，否则视频处理器会顺手做帧率转换。MP4 里读出的"原生帧率"是样本数除以轨道时长，对带音轨的文件并不准，
///   而视频处理器会按声明的帧率重排时间戳（重复帧、改时间轴），所以必须给扫描出的真实帧率。
///
/// # 返回
/// `(缩放器, 缩放后的输出类型)`。
fn open_resizer(
    src: &IMFMediaType,
    buffer: (u32, u32),
    area: (u32, u32),
    size: (u32, u32),
    rate: (u32, u32),
) -> Result<(Resizer, IMFMediaType), EditError> {
    // SAFETY: 局部有效值；MFT 在本线程创建、使用。
    unsafe {
        let mft: IMFTransform =
            CoCreateInstance(&CLSID_VideoProcessorMFT, None, CLSCTX_INPROC_SERVER)
                .map_err(|e| mf_init_err("创建视频处理器失败", e))?;
        let input = MFCreateMediaType().map_err(|e| mf_init_err("创建媒体类型失败", e))?;
        src.CopyAllItems(&input)
            .map_err(|e| mf_init_err("复制媒体类型失败", e))?;
        input
            .SetUINT64(&MF_MT_FRAME_RATE, pack_pair(rate.0, rate.1))
            .map_err(|e| mf_init_err("设置帧率失败", e))?;
        input
            .SetUINT64(&MF_MT_FRAME_SIZE, pack_pair(buffer.0, buffer.1))
            .map_err(|e| mf_init_err("设置缓冲尺寸失败", e))?;
        let _ = input.DeleteItem(&MF_MT_SAMPLE_SIZE);
        mft.SetInputType(0, &input, 0)
            .map_err(|e| mf_init_err("视频处理器不接受输入类型", e))?;
        let out = MFCreateMediaType().map_err(|e| mf_init_err("创建媒体类型失败", e))?;
        input
            .CopyAllItems(&out)
            .map_err(|e| mf_init_err("复制媒体类型失败", e))?;
        out.SetUINT64(&MF_MT_FRAME_SIZE, pack_pair(size.0, size.1))
            .map_err(|e| mf_init_err("设置缩放尺寸失败", e))?;
        // 行距和样本大小跟尺寸绑定，旧值作废
        let _ = out.DeleteItem(&MF_MT_DEFAULT_STRIDE);
        let _ = out.DeleteItem(&MF_MT_SAMPLE_SIZE);
        mft.SetOutputType(0, &out, 0)
            .map_err(|e| mf_init_err("视频处理器不接受缩放输出", e))?;
        let control: IMFVideoProcessorControl = mft
            .cast()
            .map_err(|e| mf_init_err("视频处理器不支持源区域设置", e))?;
        let source = RECT {
            left: 0,
            top: 0,
            right: area.0 as i32,
            bottom: area.1 as i32,
        };
        control
            .SetSourceRectangle(Some(&source))
            .map_err(|e| mf_init_err("设置源区域失败", e))?;
        let info = mft
            .GetOutputStreamInfo(0)
            .map_err(|e| mf_init_err("读取输出信息失败", e))?;
        let current = mft
            .GetOutputCurrentType(0)
            .map_err(|e| mf_init_err("读取输出类型失败", e))?;
        mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
            .map_err(|e| mf_init_err("启动视频处理器失败", e))?;
        mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
            .map_err(|e| mf_init_err("启动视频处理器失败", e))?;
        Ok((
            Resizer {
                mft,
                out_bytes: info.cbSize.max(size.0 * size.1 * 3 / 2),
            },
            current,
        ))
    }
}

impl Resizer {
    /// 缩放一个样本；返回处理器吐出的全部样本（正常一进一出，空表示暂时没有输出）。
    fn resize(&self, sample: &IMFSample) -> Result<Vec<IMFSample>, EditError> {
        // SAFETY: 输出样本与缓冲由本函数创建，ProcessOutput 填充后取回。
        unsafe {
            self.mft
                .ProcessInput(0, sample, 0)
                .map_err(|e| mf_err("缩放输入失败", e))?;
            let mut produced = Vec::new();
            loop {
                let out = MFCreateSample().map_err(|e| mf_err("创建输出样本失败", e))?;
                let buffer = MFCreateMemoryBuffer(self.out_bytes)
                    .map_err(|e| mf_err("创建输出缓冲失败", e))?;
                out.AddBuffer(&buffer)
                    .map_err(|e| mf_err("挂载输出缓冲失败", e))?;
                let mut data = [MFT_OUTPUT_DATA_BUFFER {
                    dwStreamID: 0,
                    pSample: std::mem::ManuallyDrop::new(Some(out)),
                    dwStatus: 0,
                    pEvents: std::mem::ManuallyDrop::new(None),
                }];
                let mut status = 0u32;
                let result = self.mft.ProcessOutput(0, &mut data, &mut status);
                let [slot] = &mut data;
                let got = std::mem::ManuallyDrop::take(&mut slot.pSample);
                drop(std::mem::ManuallyDrop::take(&mut slot.pEvents));
                match result {
                    Ok(()) => produced.extend(got),
                    Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => break,
                    Err(e) => return Err(mf_err("缩放失败", e)),
                }
            }
            Ok(produced)
        }
    }
}

/// 视频输出帧率（分子, 分母）：降 fps 取目标值，缩放沿用源帧率。
fn output_rate(target: Target, info: &ProbeInfo) -> (u32, u32) {
    match target.fps {
        Some(f) => (f, 1),
        None => (info.fps_milli.max(1000), 1000),
    }
}

/// 已配置好的读写两端。
struct Pipe {
    /// 读取器。
    reader: IMFSourceReader,
    /// 写入器。
    writer: IMFSinkWriter,
    /// 源流下标。
    streams: Streams,
    /// 写入器视频流下标。
    out_video: u32,
    /// 写入器音频流下标。
    out_audio: Option<u32>,
    /// 缩放器（不缩放时为空）。
    resizer: Option<Resizer>,
    /// 为推出缓冲真实行数而预读的第一个样本，主循环要先处理它。
    first: Option<ReadOut>,
}

/// 打开并配置读写两端（任何一步失败都视为"引擎没能启动"，可回落）。
fn open_pipe(
    input: &Path,
    tmp: &Path,
    target: Target,
    info: &ProbeInfo,
) -> Result<Pipe, EditError> {
    let scale = (target.width, target.height) != (info.width, info.height);
    let reader = open_reader(input, Processing::Basic)?;
    let streams = find_streams(&reader)?;
    select_stream(&reader, streams.video)?;
    let (rate_num, rate_den) = output_rate(target, info);
    // SAFETY: 以下均为 MF 对象创建与配置的 FFI 调用，参数都是局部有效值。
    unsafe {
        let want = MFCreateMediaType().map_err(|e| mf_init_err("创建媒体类型失败", e))?;
        want.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(|e| mf_init_err("设置主类型失败", e))?;
        want.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)
            .map_err(|e| mf_init_err("设置子类型失败", e))?;
        reader
            .SetCurrentMediaType(streams.video, None, &want)
            .map_err(|e| mf_init_err("系统解码/缩放不可用", e))?;
        let current = reader
            .GetCurrentMediaType(streams.video)
            .map_err(|e| mf_init_err("读取解码输出类型失败", e))?;
        // 音频流必须在读任何样本之前选好，否则读取器已经越过的音频样本不会补发
        let mut audio_native = None;
        if let Some(a) = streams.audio {
            let native = reader
                .GetNativeMediaType(a, 0)
                .map_err(|e| mf_init_err("读取音频类型失败", e))?;
            select_stream(&reader, a)?;
            reader
                .SetCurrentMediaType(a, None, &native)
                .map_err(|e| mf_init_err("设置音频直通失败", e))?;
            audio_native = Some(native);
        }
        // 缩放交给独立的视频处理器 MFT：SourceReader 内置的处理器会擅自重打时间戳（帧率变了、多出帧）
        // 解码器的缓冲高度常对齐到 16（1080 -> 1088），而类型里写的是显示高度；
        // 下游若按类型高度解释缓冲，色差平面的起点会错位。先取一个样本，按缓冲长度推出真实行数。
        let first = loop {
            let out = read_sample(&reader, streams.video)?;
            if out.ended() {
                return Err(EditError::unsupported("视频中没有可解码的帧"));
            }
            if out.sample.is_some() {
                break out;
            }
        };
        let total_len = first
            .sample
            .as_ref()
            .map_or(0, |s| s.GetTotalLength().unwrap_or(0)) as u64;
        let stride = u64::from(current.GetUINT32(&MF_MT_DEFAULT_STRIDE).unwrap_or(0))
            .max(u64::from(info.width));
        let buffer_rows = u32::try_from(total_len * 2 / 3 / stride).unwrap_or(info.height);
        let buffer_rows = buffer_rows.max(info.height);
        let (resizer, current) = if scale || buffer_rows != target.height {
            let (r, out_type) = open_resizer(
                &current,
                (info.width, buffer_rows),
                (info.width, info.height),
                (target.width, target.height),
                (info.fps_milli.max(1000), 1000),
            )?;
            (Some(r), out_type)
        } else {
            (None, current)
        };

        let writer = open_writer(tmp)?;
        // 输入类型 = 解码输出类型 + 目标帧率 + 补齐色彩标注
        let input_type = MFCreateMediaType().map_err(|e| mf_init_err("创建媒体类型失败", e))?;
        current
            .CopyAllItems(&input_type)
            .map_err(|e| mf_init_err("复制媒体类型失败", e))?;
        input_type
            .SetUINT64(&MF_MT_FRAME_RATE, pack_pair(rate_num, rate_den))
            .map_err(|e| mf_init_err("设置帧率失败", e))?;
        ensure_color(&input_type)?;

        let output_type = MFCreateMediaType().map_err(|e| mf_init_err("创建媒体类型失败", e))?;
        let size = (target.width, target.height);
        let fps_int = (rate_num / rate_den).max(1);
        let set = |r: windows::core::Result<()>| r.map_err(|e| mf_init_err("配置输出类型失败", e));
        set(output_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video))?;
        set(output_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264))?;
        set(output_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32))?;
        set(output_type.SetUINT64(&MF_MT_FRAME_SIZE, pack_pair(size.0, size.1)))?;
        set(output_type.SetUINT64(&MF_MT_FRAME_RATE, pack_pair(rate_num, rate_den)))?;
        set(output_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_pair(1, 1)))?;
        set(output_type.SetUINT32(
            &MF_MT_AVG_BITRATE,
            target_bitrate(size, fps_int, configured_bpp()),
        ))?;
        set(output_type.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32))?;
        set(output_type.SetUINT32(&MF_MT_MAX_KEYFRAME_SPACING, fps_int * GOP_SECONDS))?;
        for key in [
            &MF_MT_VIDEO_NOMINAL_RANGE,
            &MF_MT_YUV_MATRIX,
            &MF_MT_VIDEO_PRIMARIES,
            &MF_MT_TRANSFER_FUNCTION,
        ] {
            let v = input_type
                .GetUINT32(key)
                .map_err(|e| mf_init_err("读取色彩标注失败", e))?;
            set(output_type.SetUINT32(key, v))?;
        }
        let out_video = writer
            .AddStream(&output_type)
            .map_err(|e| mf_init_err("添加视频流失败", e))?;
        writer
            .SetInputMediaType(out_video, &input_type, None)
            .map_err(|e| mf_init_err("设置视频输入类型失败", e))?;
        verify_hardware_transform(&writer, out_video).map_err(EditError::unsupported)?;

        // 音频：原生压缩类型直通（流的选择在预读视频样本之前已完成）
        let mut out_audio = None;
        if let Some(native) = audio_native {
            let idx = writer
                .AddStream(&native)
                .map_err(|e| mf_init_err("添加音频流失败", e))?;
            writer
                .SetInputMediaType(idx, &native, None)
                .map_err(|e| mf_init_err("设置音频输入类型失败", e))?;
            out_audio = Some(idx);
        }
        writer
            .BeginWriting()
            .map_err(|e| mf_init_err("BeginWriting 失败", e))?;
        Ok(Pipe {
            reader,
            writer,
            streams,
            out_video,
            out_audio,
            resizer,
            first: Some(first),
        })
    }
}

/// 转码主体：写到中间文件。
fn transcode_to(input: &Path, tmp: &Path, op: EditOp, ctl: &TaskCtl) -> Result<u64, EditError> {
    let _session = MfSession::start()?;
    let info = scan(input)?.info;
    let target = plan_target(op, &info)?;
    // 读写两端在 Pipe 里；Pipe 先于会话释放（局部变量逆序析构）
    let mut pipe = open_pipe(input, tmp, target, &info)?;
    let (rate_num, rate_den) = output_rate(target, &info);
    let mut grid = target.fps.map(|f| FpsGrid::new(f, HNS_TIME_BASE));
    let frame_hns = HNS_PER_SECOND * i64::from(rate_den) / i64::from(rate_num);
    let total = info.frames;
    let (mut consumed, mut written) = (0u64, 0u64);
    let mut base: Option<i64> = None;
    let (mut video_done, mut audio_done) = (false, pipe.streams.audio.is_none());
    while !(video_done && audio_done) {
        if ctl.is_cancelled() {
            return Err(EditError::cancelled());
        }
        let out = match pipe.first.take() {
            Some(first) => first,
            None => read_sample(&pipe.reader, MF_SOURCE_READER_ANY_STREAM.0 as u32)?,
        };
        let is_video = out.stream == pipe.streams.video;
        if out.ended() {
            if is_video {
                video_done = true;
            } else {
                audio_done = true;
            }
            continue;
        }
        let Some(mut sample) = out.sample else {
            continue;
        };
        // SAFETY: 样本归本线程独占，改写时间戳后交给 SinkWriter。
        unsafe {
            if is_video {
                consumed += 1;
                if let Some(r) = &pipe.resizer {
                    let (time, dur) = (
                        sample.GetSampleTime().unwrap_or(out.time),
                        sample.GetSampleDuration(),
                    );
                    let outs = r.resize(&sample)?;
                    let Some(scaled) = outs.into_iter().next() else {
                        continue;
                    };
                    let _ = scaled.SetSampleTime(time);
                    if let Ok(d) = dur {
                        let _ = scaled.SetSampleDuration(d);
                    }
                    sample = scaled;
                }
                let rel = out.time - *base.get_or_insert(out.time);
                let (time, duration) = match grid.as_mut() {
                    Some(g) => match g.pick(rel + HNS_ROUNDING) {
                        Some(slot) => (slot * HNS_PER_SECOND / i64::from(rate_num), frame_hns),
                        None => {
                            ctl.report(
                                consumed.min(total.saturating_sub(1)),
                                total,
                                STAGE_TRANSCODE,
                            );
                            continue;
                        }
                    },
                    None => (rel, sample.GetSampleDuration().unwrap_or(frame_hns)),
                };
                sample
                    .SetSampleTime(time)
                    .map_err(|e| mf_err("改写时间戳失败", e))?;
                sample
                    .SetSampleDuration(duration)
                    .map_err(|e| mf_err("改写时长失败", e))?;
                pipe.writer
                    .WriteSample(pipe.out_video, &sample)
                    .map_err(|e| mf_err("写入视频样本失败", e))?;
                written += 1;
                ctl.report(
                    consumed.min(total.saturating_sub(1)),
                    total,
                    STAGE_TRANSCODE,
                );
            } else if let Some(idx) = pipe.out_audio {
                let time = (out.time - base.unwrap_or(0)).max(0);
                sample
                    .SetSampleTime(time)
                    .map_err(|e| mf_err("改写音频时间戳失败", e))?;
                pipe.writer
                    .WriteSample(idx, &sample)
                    .map_err(|e| mf_err("写入音频样本失败", e))?;
            }
        }
    }
    // SAFETY: 结束写入并封装 MP4。
    unsafe { pipe.writer.Finalize() }.map_err(|e| mf_err("封装输出文件失败", e))?;
    ctl.report(total, total, STAGE_TRANSCODE);
    Ok(written)
}
