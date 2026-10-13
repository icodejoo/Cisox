//! 模型加载提速：`model_type` 解析与 encoder 预优化缓存。
//!
//! 背景（见 `docs/research/stt-bilingual-model-load.md`）：sherpa 在 `model_type` 为空时会先建一个
//! 临时 ORT 会话读元数据，等于把 encoder 载两遍；而 encoder 的计算图有上万个碎节点，ORT 建会话是单线程逐节点
//! 处理，占了加载时间的绝大部分。两个手段：
//! 1. 直接告诉 sherpa `model_type`（来源：模型目录里的 `model.json`，其次是缓存元数据），省掉临时会话；
//! 2. 用 ORT 把 encoder 离线优化一次（EXTENDED 级别，批量维 `N` 固定为 1）落盘到 `<模型目录>/.opt/`，
//!    之后直接载优化后的文件。生成由后台低优先级子进程（`snow-stt --optimize <模型目录>`）完成，
//!    生成期间本次照常用原文件；任何一步失败都回退原文件，且同一份源文件不会反复重试。
//!
//! 缓存与 ORT（构建信息串加 dll 大小）、源 encoder（文件名、大小、修改时间）绑定，变化即视为失效。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

/// 模型目录里的元数据文件名（由主程序下载安装时写入）。
pub const MODEL_META_FILE: &str = "model.json";
/// 元数据里的 `model_type` 键（也是 ONNX 自定义元数据里的键名）。
const MODEL_TYPE_KEY: &str = "model_type";
/// 流式 transducer 在 sherpa 里认的 `model_type`（见 sherpa-onnx `online-transducer-model.cc`）。
pub const ONLINE_TRANSDUCER_TYPES: [&str; 5] = [
    "conformer",
    "ebranchformer",
    "lstm",
    "zipformer",
    "zipformer2",
];
/// 离线 transducer 在 sherpa 里认的 `model_type`（见 sherpa-onnx `offline-recognizer-impl.cc`）。
pub const OFFLINE_TRANSDUCER_TYPES: [&str; 2] = ["transducer", "nemo_transducer"];

/// 缓存子目录名（位于模型目录下，随模型目录一起被替换 / 删除）。
pub const OPT_DIR: &str = ".opt";
/// 优化后的 encoder 文件名。
const OPT_ENCODER_FILE: &str = "encoder.opt.onnx";
/// 生成过程中的临时 encoder 文件名（生成成功才改名为正式文件）。
const OPT_TMP_FILE: &str = "encoder.opt.tmp.onnx";
/// 缓存元数据文件名（最后写入，有它才算缓存可用 / 已判失败）。
const OPT_META_FILE: &str = "encoder.opt.json";
/// 生成锁文件名（防止多个 worker 同时各起一个生成进程）。
const OPT_LOCK_FILE: &str = "encoder.opt.lock";
/// 锁文件超过多久视为陈旧（生成进程被杀时遗留）。
const LOCK_STALE: Duration = Duration::from_secs(15 * 60);
/// 指纹取首尾各多少字节做哈希。
const EDGE_BYTES: usize = 64 * 1024;
/// FNV-1a 64 位初始值。
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64 位乘数。
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
/// 缓存元数据格式版本。
const CACHE_SCHEMA: u64 = 2;
/// 元数据里记录的优化级别标记（对应 ORT 的 `ORT_ENABLE_EXTENDED`）。
const OPT_LEVEL_TAG: &str = "extended";
/// 要固定的动态维度名（icefall 导出的 encoder 批量维叫 `N`）。
const FREE_DIM_NAME: &str = "N";
/// 固定后的批量大小（worker 只有一路流）。
const FREE_DIM_SIZE: i64 = 1;
/// 元数据里记录的维度固定标记。
const FREE_DIM_TAG: &str = "N=1";
/// 生成时 ORT 的线程数（建会话本来就是单线程，多给也没用，且不抢听写的 CPU）。
const GEN_THREADS: usize = 1;
/// 元数据状态：缓存可用。
const STATE_READY: &str = "ready";
/// 元数据状态：生成或校验失败，不再重试。
const STATE_FAILED: &str = "failed";
/// onnxruntime 动态库文件名（与 exe 同目录，sherpa 也加载这一份）。
const ORT_DLL_NAME: &str = "onnxruntime.dll";
/// 覆盖 onnxruntime 动态库路径的环境变量（与 snow-table 一致）。
const ENV_ORT_DYLIB: &str = "SNOW_ORT_DYLIB";
/// 后台生成子进程的命令行开关（后面跟模型目录）。
pub const ARG_OPTIMIZE: &str = "--optimize";
/// Windows 进程创建标志：不弹控制台窗口。
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// Windows 进程创建标志：空闲优先级，听写永远优先。
/// 不用 `PROCESS_MODE_BACKGROUND_BEGIN`：它连带压低 IO / 内存优先级，实测生成从约 5 s 拖到约 88 s。
#[cfg(windows)]
const IDLE_PRIORITY_CLASS: u32 = 0x0000_0040;

/// 加载选项（测试 / 对比实验可单独关掉各项，生产默认全开）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoadOpts {
    /// 是否把 `model_type` 交给 sherpa（关掉即旧行为：sherpa 自己再载一遍 encoder 读元数据）。
    pub model_type: bool,
    /// 是否使用 / 生成 encoder 预优化缓存。
    pub opt_cache: bool,
}

impl Default for LoadOpts {
    /// 生产默认：两项都开。
    fn default() -> Self {
        Self {
            model_type: true,
            opt_cache: true,
        }
    }
}

/// 加载前的决策结果：交给 sherpa 的 encoder 路径与 `model_type`，以及是否需要后台生成缓存。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadPlan {
    /// 交给 sherpa 的 encoder 路径（命中缓存时是优化后的文件，否则是原文件）。
    pub encoder: PathBuf,
    /// 交给 sherpa 的 `model_type`；`None` 表示不设（sherpa 自己探测）。
    pub model_type: Option<String>,
    /// 是否命中了预优化缓存。
    pub cache_hit: bool,
    /// 本次结束后是否应拉起后台生成（没有缓存、没有失败记录、没人在生成、ORT 可用）。
    pub want_generation: bool,
}

/// 源 encoder 的指纹：文件名、字节数与修改时间（毫秒）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct SourceStamp {
    /// 文件名（不含目录）。
    name: String,
    /// 字节数。
    len: u64,
    /// 修改时间（自 1970 起的毫秒数，读不到按 0）。
    mtime_ms: u64,
}

/// 优化产物的指纹：字节数、修改时间与首尾各 [`EDGE_BYTES`] 字节的哈希。
///
/// 加载时逐次校验整份 150MB 的哈希太慢（会吃掉提速收益），所以用大小加时间加首尾哈希：
/// 截断、被覆盖、被改写都能发现。sherpa 在遇到损坏模型时可能直接结束进程，不能指望事后回退。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct OptStamp {
    /// 字节数。
    len: u64,
    /// 修改时间（毫秒）。
    mtime_ms: u64,
    /// 首尾字节的 FNV-1a 64 位哈希（十六进制）。
    edge_hash: String,
}

/// 缓存元数据（`.opt/encoder.opt.json`）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct CacheMeta {
    /// 状态：`ready` 或 `failed`。
    state: String,
    /// 生成时的 ORT 指纹。
    ort: String,
    /// 源 encoder 指纹。
    source: SourceStamp,
    /// 优化后文件指纹（`failed` 时为默认空值）。
    opt: OptStamp,
    /// 从 encoder 元数据里读到的 `model_type`（可能没有）。
    model_type: Option<String>,
    /// 失败原因（仅 `failed`）。
    error: Option<String>,
}

/// 一次优化的产出信息。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OptimizeInfo {
    /// 源 encoder 元数据里的 `model_type`。
    pub model_type: Option<String>,
}

/// 生成结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenOutcome {
    /// 刚生成完。
    Generated,
    /// 缓存已经是最新的，什么都没做。
    UpToDate,
}

/// encoder 优化器：把源 encoder 优化后写到目标路径（生产用 [`OrtOptimizer`]，测试用假实现）。
pub trait EncoderOptimizer {
    /// 优化并落盘，同时校验产物可用。
    ///
    /// # 参数
    /// - `src`：源 encoder。
    /// - `dst`：产物路径（临时文件，成功后由调用方改名）。
    ///
    /// # 返回
    /// 产出信息；优化或校验失败返回原因。
    fn optimize(&self, src: &Path, dst: &Path) -> Result<OptimizeInfo, String>;
}

/// 读取模型目录 `model.json` 里的 `model_type`，只接受 `allowed` 里的取值。
///
/// # 参数
/// - `model_dir`：模型目录。
/// - `allowed`：该类型 sherpa 认的取值清单（填错会让创建失败，所以先过白名单）。
///
/// # 返回
/// 合法的取值；文件不存在、解析失败、字段缺失或取值不在白名单里都返回 `None`。
///
/// # 示例
/// ```ignore
/// let t = read_model_type(Path::new("models/x-asr"), &ONLINE_TRANSDUCER_TYPES);
/// assert_eq!(t.as_deref(), Some("zipformer2"));
/// ```
pub fn read_model_type(model_dir: &Path, allowed: &[&str]) -> Option<String> {
    let text = fs::read_to_string(model_dir.join(MODEL_META_FILE)).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    allowed_type(value.get(MODEL_TYPE_KEY)?.as_str()?, allowed)
}

/// 取值在白名单里才返回。
fn allowed_type(raw: &str, allowed: &[&str]) -> Option<String> {
    let t = raw.trim();
    allowed.contains(&t).then(|| t.to_string())
}

/// 读源文件指纹。
fn stamp_of(path: &Path) -> Result<SourceStamp, String> {
    let meta = fs::metadata(path).map_err(|e| format!("读取 {} 失败: {e}", path.display()))?;
    let mtime_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as u64);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(SourceStamp {
        name,
        len: meta.len(),
        mtime_ms,
    })
}

/// 读优化产物指纹。
fn opt_stamp_of(path: &Path) -> Result<OptStamp, String> {
    use std::io::{Read, Seek, SeekFrom};
    let err = |e: std::io::Error| format!("读取 {} 失败: {e}", path.display());
    let mut file = fs::File::open(path).map_err(err)?;
    let meta = file.metadata().map_err(err)?;
    let mtime_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as u64);
    let len = meta.len();
    let mut hash = FNV_OFFSET;
    let mut buf = vec![0u8; EDGE_BYTES.min(len as usize)];
    file.read_exact(&mut buf).map_err(err)?;
    hash = fnv1a(hash, &buf);
    if len as usize > EDGE_BYTES {
        file.seek(SeekFrom::Start(len - buf.len() as u64))
            .map_err(err)?;
        file.read_exact(&mut buf).map_err(err)?;
        hash = fnv1a(hash, &buf);
    }
    Ok(OptStamp {
        len,
        mtime_ms,
        edge_hash: format!("{hash:016x}"),
    })
}

/// FNV-1a 64 位哈希（只用来发现文件被改动，不涉及安全）。
fn fnv1a(mut hash: u64, bytes: &[u8]) -> u64 {
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// 缓存目录。
fn opt_dir(model_dir: &Path) -> PathBuf {
    model_dir.join(OPT_DIR)
}

/// 元数据序列化。
fn meta_to_json(meta: &CacheMeta) -> Value {
    json!({
        "schema": CACHE_SCHEMA,
        "state": meta.state,
        "ort": meta.ort,
        "level": OPT_LEVEL_TAG,
        "free_dim": FREE_DIM_TAG,
        "source": {
            "name": meta.source.name,
            "len": meta.source.len,
            "mtime_ms": meta.source.mtime_ms,
        },
        "opt": {
            "len": meta.opt.len,
            "mtime_ms": meta.opt.mtime_ms,
            "edge_hash": meta.opt.edge_hash,
        },
        "model_type": meta.model_type,
        "error": meta.error,
    })
}

/// 读缓存元数据；不存在、损坏、格式版本不符都当没有。
fn read_meta(model_dir: &Path) -> Option<CacheMeta> {
    let text = fs::read_to_string(opt_dir(model_dir).join(OPT_META_FILE)).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    if v.get("schema")?.as_u64()? != CACHE_SCHEMA {
        return None;
    }
    let source = v.get("source")?;
    Some(CacheMeta {
        state: v.get("state")?.as_str()?.to_string(),
        ort: v.get("ort")?.as_str()?.to_string(),
        source: SourceStamp {
            name: source.get("name")?.as_str()?.to_string(),
            len: source.get("len")?.as_u64()?,
            mtime_ms: source.get("mtime_ms")?.as_u64()?,
        },
        opt: {
            let opt = v.get("opt")?;
            OptStamp {
                len: opt.get("len")?.as_u64()?,
                mtime_ms: opt.get("mtime_ms")?.as_u64()?,
                edge_hash: opt.get("edge_hash")?.as_str()?.to_string(),
            }
        },
        model_type: v
            .get(MODEL_TYPE_KEY)
            .and_then(Value::as_str)
            .map(str::to_string),
        error: v.get("error").and_then(Value::as_str).map(str::to_string),
    })
}

/// 原子写缓存元数据（先写临时文件再改名）。
fn write_meta(model_dir: &Path, meta: &CacheMeta) -> Result<(), String> {
    let dir = opt_dir(model_dir);
    let tmp = dir.join(format!("{OPT_META_FILE}.tmp"));
    let text = serde_json::to_string_pretty(&meta_to_json(meta)).map_err(|e| e.to_string())?;
    fs::write(&tmp, text).map_err(|e| format!("写缓存元数据失败: {e}"))?;
    fs::rename(&tmp, dir.join(OPT_META_FILE)).map_err(|e| format!("写缓存元数据失败: {e}"))
}

/// 生成锁是否有人持有（存在且没过期）。
fn lock_is_fresh(model_dir: &Path) -> bool {
    let Ok(meta) = fs::metadata(opt_dir(model_dir).join(OPT_LOCK_FILE)) else {
        return false;
    };
    meta.modified()
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_none_or(|age| age < LOCK_STALE)
}

/// 生成锁守卫：析构时删除锁文件。
struct LockGuard(PathBuf);

impl Drop for LockGuard {
    /// 释放锁。
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// 抢生成锁；有人在生成（锁新鲜）时返回 `None`，陈旧的锁会被清掉重抢。
fn acquire_lock(model_dir: &Path) -> Option<LockGuard> {
    let path = opt_dir(model_dir).join(OPT_LOCK_FILE);
    for _ in 0..2 {
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => return Some(LockGuard(path)),
            Err(_) if lock_is_fresh(model_dir) => return None,
            Err(_) => {
                let _ = fs::remove_file(&path);
            }
        }
    }
    None
}

/// 加载前的决策：选 encoder 路径与 `model_type`，并判断是否需要后台生成缓存。
///
/// 规则：`model_type` 先取 `model.json`，其次取缓存元数据（源文件没变时有效）；缓存命中要求
/// 状态 `ready`、ORT 指纹一致、源指纹一致、优化文件大小一致；状态 `failed` 且指纹一致时不再重试生成。
///
/// # 参数
/// - `model_dir`：模型目录。
/// - `encoder_src`：原 encoder 文件。
/// - `fingerprint`：当前 ORT 指纹；`None` 表示 ORT 不可用（不使用也不生成缓存）。
/// - `allowed_types`：`model_type` 白名单。
/// - `opts`：加载选项。
///
/// # 返回
/// 加载计划。
///
/// # 示例
/// ```ignore
/// let plan = plan_encoder(dir, &enc, Some("fp"), &ONLINE_TRANSDUCER_TYPES, LoadOpts::default());
/// let encoder_for_sherpa = plan.encoder;
/// ```
pub fn plan_encoder(
    model_dir: &Path,
    encoder_src: &Path,
    fingerprint: Option<&str>,
    allowed_types: &[&str],
    opts: LoadOpts,
) -> LoadPlan {
    let from_manifest = opts
        .model_type
        .then(|| read_model_type(model_dir, allowed_types))
        .flatten();
    let mut plan = LoadPlan {
        encoder: encoder_src.to_path_buf(),
        model_type: from_manifest,
        cache_hit: false,
        want_generation: false,
    };
    let Some(fp) = fingerprint.filter(|_| opts.opt_cache) else {
        return plan;
    };
    let Ok(stamp) = stamp_of(encoder_src) else {
        return plan;
    };
    let meta = read_meta(model_dir).filter(|m| m.source == stamp);
    if opts.model_type && plan.model_type.is_none() {
        plan.model_type = meta
            .as_ref()
            .and_then(|m| m.model_type.as_deref())
            .and_then(|t| allowed_type(t, allowed_types));
    }
    match meta {
        Some(m) if m.ort == fp && m.state == STATE_FAILED => {}
        Some(m) if m.ort == fp && m.state == STATE_READY => {
            let opt = opt_dir(model_dir).join(OPT_ENCODER_FILE);
            if m.opt.len > 0 && opt_stamp_of(&opt).is_ok_and(|now| now == m.opt) {
                plan.encoder = opt;
                plan.cache_hit = true;
            } else {
                plan.want_generation = !lock_is_fresh(model_dir);
            }
        }
        _ => plan.want_generation = !lock_is_fresh(model_dir),
    }
    plan
}

/// 缓存命中但 sherpa 仍创建失败时调用：把缓存标成失败，之后不再使用也不再重试。
///
/// # 参数
/// - `model_dir`：模型目录。
/// - `encoder_src`：原 encoder 文件。
/// - `fingerprint`：当前 ORT 指纹。
/// - `reason`：失败原因。
pub fn mark_failed(model_dir: &Path, encoder_src: &Path, fingerprint: &str, reason: &str) {
    let Ok(source) = stamp_of(encoder_src) else {
        return;
    };
    let prior = read_meta(model_dir).filter(|m| m.source == source);
    let meta = CacheMeta {
        state: STATE_FAILED.into(),
        ort: fingerprint.into(),
        source,
        opt: OptStamp::default(),
        model_type: prior.and_then(|m| m.model_type),
        error: Some(reason.into()),
    };
    let _ = fs::create_dir_all(opt_dir(model_dir));
    let _ = write_meta(model_dir, &meta);
}

/// 生成预优化缓存（前台阻塞；生产上由后台子进程调用）。
///
/// 流程：抢锁 → 已是最新则直接返回 → 优化到临时文件 → 成功后改名为正式文件 → 最后写元数据。
/// 优化器失败会写 `failed` 记录（同一份源文件与 ORT 不再重试）并清掉临时文件。
///
/// # 参数
/// - `model_dir`：模型目录。
/// - `encoder_src`：源 encoder 文件。
/// - `fingerprint`：当前 ORT 指纹。
/// - `allowed_types`：`model_type` 白名单（过滤从 ONNX 元数据里读到的值）。
/// - `optimizer`：优化器。
///
/// # 返回
/// 生成结果；有人正在生成、目录不可写或优化失败时返回原因。
///
/// # 示例
/// ```ignore
/// let opt = OrtOptimizer::new(locate_ort_dll());
/// generate(dir, &enc, &fp, &ONLINE_TRANSDUCER_TYPES, &opt)?;
/// ```
pub fn generate(
    model_dir: &Path,
    encoder_src: &Path,
    fingerprint: &str,
    allowed_types: &[&str],
    optimizer: &dyn EncoderOptimizer,
) -> Result<GenOutcome, String> {
    let dir = opt_dir(model_dir);
    fs::create_dir_all(&dir).map_err(|e| format!("无法创建缓存目录 {}: {e}", dir.display()))?;
    let _lock = acquire_lock(model_dir).ok_or_else(|| "已有进程正在生成缓存".to_string())?;
    let source = stamp_of(encoder_src)?;
    let plan = plan_encoder(
        model_dir,
        encoder_src,
        Some(fingerprint),
        allowed_types,
        LoadOpts::default(),
    );
    let state = read_meta(model_dir).filter(|m| m.source == source && m.ort == fingerprint);
    if plan.cache_hit || state.is_some_and(|m| m.state == STATE_FAILED) {
        return Ok(GenOutcome::UpToDate);
    }
    let tmp = dir.join(OPT_TMP_FILE);
    let _ = fs::remove_file(&tmp);
    let info = match optimizer.optimize(encoder_src, &tmp) {
        Ok(info) => info,
        Err(why) => {
            let _ = fs::remove_file(&tmp);
            mark_failed(model_dir, encoder_src, fingerprint, &why);
            return Err(why);
        }
    };
    let dst = dir.join(OPT_ENCODER_FILE);
    // 先让旧元数据失效，再换文件，最后写新元数据：任何时刻读到的都是一致的状态
    let _ = fs::remove_file(dir.join(OPT_META_FILE));
    let _ = fs::remove_file(&dst);
    fs::rename(&tmp, &dst).map_err(|e| format!("缓存文件改名失败: {e}"))?;
    let opt = opt_stamp_of(&dst)?;
    let model_type = info
        .model_type
        .as_deref()
        .and_then(|t| allowed_type(t, allowed_types));
    write_meta(
        model_dir,
        &CacheMeta {
            state: STATE_READY.into(),
            ort: fingerprint.into(),
            source,
            opt,
            model_type,
            error: None,
        },
    )?;
    Ok(GenOutcome::Generated)
}

/// 找 onnxruntime 动态库：环境变量优先，否则取 exe 同目录那一份（sherpa 加载的也是它）。
pub fn locate_ort_dll() -> PathBuf {
    if let Some(p) = std::env::var_os(ENV_ORT_DYLIB) {
        return PathBuf::from(p);
    }
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join(ORT_DLL_NAME)))
        .unwrap_or_else(|| PathBuf::from(ORT_DLL_NAME))
}

/// 当前 ORT 的指纹（构建信息串加 dll 字节数）；动态库加载失败返回 `None`。
///
/// 只读取 dll 的构建信息，不创建 ORT 环境，开销是毫秒级。
///
/// # 参数
/// - `dll`：onnxruntime 动态库路径。
pub fn ort_fingerprint(dll: &Path) -> Option<String> {
    let len = fs::metadata(dll).ok()?.len();
    // ort 在版本不符等情况下可能 panic，加载提速不能因此拖垮识别
    std::panic::catch_unwind(|| {
        ort::init_from(dll).ok()?;
        Some(format!("{}|len={len}", ort::info()))
    })
    .ok()
    .flatten()
}

/// 用 ORT 做离线优化的生产实现。
pub struct OrtOptimizer {
    /// onnxruntime 动态库路径。
    dll: PathBuf,
}

impl OrtOptimizer {
    /// 创建优化器。
    ///
    /// # 参数
    /// - `dll`：onnxruntime 动态库路径。
    pub fn new(dll: PathBuf) -> Self {
        Self { dll }
    }
}

/// 把 ORT 错误转成中文前缀的单行文本。
fn ort_err(what: &str, e: impl std::fmt::Display) -> String {
    format!("{what}: {e}")
}

/// 取会话的输入、输出名（用来核对优化前后接口一致）。
fn io_names(session: &ort::session::Session) -> (Vec<String>, Vec<String>) {
    let names = |v: &[ort::value::Outlet]| v.iter().map(|o| o.name().to_string()).collect();
    (names(session.inputs()), names(session.outputs()))
}

impl EncoderOptimizer for OrtOptimizer {
    /// 单线程、EXTENDED 级别、固定批量为 1，优化后落盘；再用 ORT 载入产物核对输入输出名。
    fn optimize(&self, src: &Path, dst: &Path) -> Result<OptimizeInfo, String> {
        use ort::session::Session;
        use ort::session::builder::GraphOptimizationLevel;

        ort::init_from(&self.dll)
            .map_err(|e| ort_err("加载 onnxruntime 失败", e))?
            .with_telemetry(false)
            .commit();
        let cfg = "配置 ORT 会话失败";
        let mut builder = Session::builder().map_err(|e| ort_err(cfg, e))?;
        builder = builder
            .with_intra_threads(GEN_THREADS)
            .map_err(|e| ort_err(cfg, e))?;
        builder = builder
            .with_inter_threads(GEN_THREADS)
            .map_err(|e| ort_err(cfg, e))?;
        builder = builder
            .with_optimization_level(GraphOptimizationLevel::Level2)
            .map_err(|e| ort_err(cfg, e))?;
        builder = builder
            .with_dimension_override(FREE_DIM_NAME, FREE_DIM_SIZE)
            .map_err(|e| ort_err(cfg, e))?;
        builder = builder
            .with_optimized_model_path(dst)
            .map_err(|e| ort_err(cfg, e))?;
        let original = builder
            .commit_from_file(src)
            .map_err(|e| ort_err("ORT 优化失败", e))?;
        let model_type = original
            .metadata()
            .ok()
            .and_then(|m| m.custom(MODEL_TYPE_KEY));
        let want = io_names(&original);
        drop(original);
        // 校验：产物必须能被 ORT 载入（用默认优化级别，等同 sherpa 真实加载时的做法）
        let mut check_builder = Session::builder().map_err(|e| ort_err(cfg, e))?;
        check_builder = check_builder
            .with_intra_threads(GEN_THREADS)
            .map_err(|e| ort_err(cfg, e))?;
        let checked = check_builder
            .commit_from_file(dst)
            .map_err(|e| ort_err("优化产物无法载入", e))?;
        if io_names(&checked) != want {
            return Err("优化产物的输入输出与原模型不一致".into());
        }
        Ok(OptimizeInfo { model_type })
    }
}

/// `--optimize <模型目录>` 的入口：前台生成缓存并打印一行结果，返回进程退出码。
///
/// # 参数
/// - `model_dir`：模型目录。
///
/// # 返回
/// 退出码：成功（含"已是最新"）为 0，失败为 1。
pub fn run_optimize_cli(model_dir: &Path) -> i32 {
    let started = std::time::Instant::now();
    let result = (|| {
        let files = crate::backend::discover_model(model_dir)?;
        let dll = locate_ort_dll();
        let fingerprint =
            ort_fingerprint(&dll).ok_or_else(|| format!("无法加载 {}", dll.display()))?;
        generate(
            model_dir,
            Path::new(&files.encoder),
            &fingerprint,
            &ONLINE_TRANSDUCER_TYPES,
            &OrtOptimizer::new(dll),
        )
    })();
    match result {
        Ok(outcome) => {
            println!(
                "optimize: {outcome:?} in {} ms",
                started.elapsed().as_millis()
            );
            0
        }
        Err(why) => {
            println!("optimize: failed: {why}");
            1
        }
    }
}

/// 把本进程的标准输入 / 输出 / 错误句柄设为不可继承。
///
/// 否则后台子进程会继承 worker 与宿主之间的管道句柄，宿主读管道读到 EOF 要等子进程跑完
/// （实测会让 worker 退出后管道多挂约 5 s）。失败忽略。
#[cfg(windows)]
fn make_std_handles_non_inheritable() {
    use windows::Win32::Foundation::{HANDLE_FLAG_INHERIT, HANDLE_FLAGS, SetHandleInformation};
    use windows::Win32::System::Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };
    for id in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: 只读取本进程标准句柄并修改其继承标志，不转移所有权，也不关闭句柄
        unsafe {
            if let Ok(handle) = GetStdHandle(id)
                && !handle.is_invalid()
            {
                let _ = SetHandleInformation(handle, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0));
            }
        }
    }
}

/// 拉起后台生成子进程（空闲优先级、无窗口、不继承管道，父进程退出后继续跑）。
///
/// # 参数
/// - `model_dir`：模型目录。
///
/// # 返回
/// 拉起成功返回 `Ok`；找不到自身路径或创建进程失败返回原因（调用方忽略即可，不影响识别）。
pub fn spawn_background(model_dir: &Path) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("找不到自身路径: {e}"))?;
    let mut cmd = Command::new(&exe);
    cmd.arg(ARG_OPTIMIZE)
        .arg(model_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(dir) = exe.parent() {
        cmd.current_dir(dir);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        make_std_handles_non_inheritable();
        cmd.creation_flags(CREATE_NO_WINDOW | IDLE_PRIORITY_CLASS);
    }
    cmd.spawn()
        .map(|_| ())
        .map_err(|e| format!("拉起后台优化失败: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// 测试用 ORT 指纹。
    const FP: &str = "ort-test|len=1";

    /// 临时目录，析构时清理。
    struct TempDir(PathBuf);

    impl TempDir {
        /// 在系统临时目录下建一个唯一目录。
        fn new() -> Self {
            static N: AtomicU32 = AtomicU32::new(0);
            let p = std::env::temp_dir().join(format!(
                "snow-stt-optcache-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }

    impl Drop for TempDir {
        /// 清理目录。
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// 假优化器：按预设成功或失败，并记录调用次数。
    struct Fake {
        /// 是否成功。
        ok: bool,
        /// 调用次数。
        calls: Cell<u32>,
        /// 成功时报告的 `model_type`。
        model_type: Option<String>,
    }

    impl Fake {
        /// 成功的假优化器。
        fn ok(model_type: Option<&str>) -> Self {
            Self {
                ok: true,
                calls: Cell::new(0),
                model_type: model_type.map(str::to_string),
            }
        }

        /// 失败的假优化器。
        fn failing() -> Self {
            Self {
                ok: false,
                ..Self::ok(None)
            }
        }
    }

    impl EncoderOptimizer for Fake {
        /// 成功时写一个小文件，失败时留下半截临时文件（用来验证清理）。
        fn optimize(&self, _src: &Path, dst: &Path) -> Result<OptimizeInfo, String> {
            self.calls.set(self.calls.get() + 1);
            if !self.ok {
                fs::write(dst, b"half").unwrap();
                return Err("故意失败".into());
            }
            fs::write(dst, b"optimized-encoder").unwrap();
            Ok(OptimizeInfo {
                model_type: self.model_type.clone(),
            })
        }
    }

    /// 造一个带 encoder 的模型目录，返回（目录守卫，encoder 路径）。
    fn model_dir(meta: Option<&str>) -> (TempDir, PathBuf) {
        let t = TempDir::new();
        let enc = t.0.join("encoder.int8.onnx");
        fs::write(&enc, b"original-encoder-bytes").unwrap();
        if let Some(m) = meta {
            fs::write(t.0.join(MODEL_META_FILE), m).unwrap();
        }
        (t, enc)
    }

    /// 以默认选项、给定指纹做一次决策。
    fn plan(dir: &Path, enc: &Path, fp: Option<&str>) -> LoadPlan {
        plan_encoder(dir, enc, fp, &ONLINE_TRANSDUCER_TYPES, LoadOpts::default())
    }

    /// 生成一次缓存（假优化器成功）。
    fn generate_ok(dir: &Path, enc: &Path, fp: &str, fake: &Fake) -> GenOutcome {
        generate(dir, enc, fp, &ONLINE_TRANSDUCER_TYPES, fake).unwrap()
    }

    #[test]
    fn model_type_comes_from_manifest_and_is_whitelisted() {
        let (t, _) = model_dir(Some(r#"{"model_type":"zipformer2"}"#));
        assert_eq!(
            read_model_type(&t.0, &ONLINE_TRANSDUCER_TYPES).as_deref(),
            Some("zipformer2")
        );
        // 离线白名单里没有 zipformer2：不能把流式的值用到离线上
        assert_eq!(read_model_type(&t.0, &OFFLINE_TRANSDUCER_TYPES), None);
        let (t, _) = model_dir(Some(r#"{"model_type":" transducer "}"#));
        assert_eq!(
            read_model_type(&t.0, &OFFLINE_TRANSDUCER_TYPES).as_deref(),
            Some("transducer")
        );
        // 缺字段、非字符串、不认识的取值、坏 JSON、没有文件，一律当没有
        for meta in [
            Some(r#"{"kind":"online-transducer"}"#),
            Some(r#"{"model_type":5}"#),
            Some(r#"{"model_type":"bogus"}"#),
            Some("{oops"),
            None,
        ] {
            let (t, _) = model_dir(meta);
            assert_eq!(
                read_model_type(&t.0, &ONLINE_TRANSDUCER_TYPES),
                None,
                "{meta:?}"
            );
        }
    }

    #[test]
    fn plan_without_cache_asks_for_generation() {
        let (t, enc) = model_dir(Some(r#"{"model_type":"zipformer2"}"#));
        let p = plan(&t.0, &enc, Some(FP));
        assert_eq!(p.encoder, enc);
        assert_eq!(p.model_type.as_deref(), Some("zipformer2"));
        assert!(!p.cache_hit && p.want_generation);
        // ORT 不可用：不用也不生成缓存，但 model_type 照常给
        let p = plan(&t.0, &enc, None);
        assert!(!p.cache_hit && !p.want_generation);
        assert_eq!(p.model_type.as_deref(), Some("zipformer2"));
    }

    #[test]
    fn options_switch_features_off() {
        let (t, enc) = model_dir(Some(r#"{"model_type":"zipformer2"}"#));
        let off = LoadOpts {
            model_type: false,
            opt_cache: false,
        };
        let p = plan_encoder(&t.0, &enc, Some(FP), &ONLINE_TRANSDUCER_TYPES, off);
        assert_eq!(
            (p.model_type, p.cache_hit, p.want_generation),
            (None, false, false)
        );
    }

    #[test]
    fn generated_cache_is_hit_and_provides_model_type() {
        // model.json 里没有 model_type（旧安装）：靠缓存元数据补上
        let (t, enc) = model_dir(Some(r#"{"id":"x"}"#));
        let fake = Fake::ok(Some("zipformer2"));
        assert_eq!(generate_ok(&t.0, &enc, FP, &fake), GenOutcome::Generated);
        let p = plan(&t.0, &enc, Some(FP));
        assert!(p.cache_hit && !p.want_generation);
        assert_eq!(p.encoder, t.0.join(OPT_DIR).join(OPT_ENCODER_FILE));
        assert_eq!(p.model_type.as_deref(), Some("zipformer2"));
        // 锁与临时文件都已清掉
        assert!(!t.0.join(OPT_DIR).join(OPT_LOCK_FILE).exists());
        assert!(!t.0.join(OPT_DIR).join(OPT_TMP_FILE).exists());
        // 再次生成：已是最新，不再调用优化器
        assert_eq!(generate_ok(&t.0, &enc, FP, &fake), GenOutcome::UpToDate);
        assert_eq!(fake.calls.get(), 1);
    }

    #[test]
    fn invalid_model_type_from_onnx_is_dropped() {
        let (t, enc) = model_dir(None);
        generate_ok(&t.0, &enc, FP, &Fake::ok(Some("EncDecCTCModel")));
        let p = plan(&t.0, &enc, Some(FP));
        assert!(p.cache_hit);
        assert_eq!(p.model_type, None);
    }

    #[test]
    fn changed_source_or_ort_invalidates_cache() {
        let (t, enc) = model_dir(None);
        generate_ok(&t.0, &enc, FP, &Fake::ok(Some("zipformer2")));
        // ORT 变了：缓存失效、要重建，但源没变，model_type 仍可用
        let p = plan(&t.0, &enc, Some("ort-other|len=2"));
        assert!(!p.cache_hit && p.want_generation);
        assert_eq!(p.encoder, enc);
        assert_eq!(p.model_type.as_deref(), Some("zipformer2"));
        // 源文件变了（大小不同）：缓存与其中的 model_type 都失效
        fs::write(&enc, b"a-different-encoder-with-other-size").unwrap();
        let p = plan(&t.0, &enc, Some(FP));
        assert!(!p.cache_hit && p.want_generation);
        assert_eq!(p.model_type, None);
        // 重建后又命中
        generate_ok(&t.0, &enc, FP, &Fake::ok(None));
        assert!(plan(&t.0, &enc, Some(FP)).cache_hit);
    }

    #[test]
    fn modified_time_change_invalidates_cache() {
        let (t, enc) = model_dir(None);
        generate_ok(&t.0, &enc, FP, &Fake::ok(None));
        let f = fs::OpenOptions::new().write(true).open(&enc).unwrap();
        f.set_modified(SystemTime::now() + Duration::from_secs(3600))
            .unwrap();
        drop(f);
        assert!(!plan(&t.0, &enc, Some(FP)).cache_hit);
    }

    #[test]
    fn same_length_corruption_is_detected() {
        // 大于 2 个边缘块的假产物：改首、改尾、只改修改时间，都要判失效
        let (t, enc) = model_dir(None);
        generate_ok(&t.0, &enc, FP, &Fake::ok(None));
        let opt = t.0.join(OPT_DIR).join(OPT_ENCODER_FILE);
        let big = vec![7u8; EDGE_BYTES * 3];
        fs::write(&opt, &big).unwrap();
        // 重新登记指纹，模拟“正常生成的大文件”
        let mut meta = read_meta(&t.0).unwrap();
        meta.opt = opt_stamp_of(&opt).unwrap();
        write_meta(&t.0, &meta).unwrap();
        assert!(plan(&t.0, &enc, Some(FP)).cache_hit);
        for pos in [3, big.len() - 3] {
            let mut bad = big.clone();
            bad[pos] ^= 0xff;
            let time = fs::metadata(&opt).unwrap().modified().unwrap();
            fs::write(&opt, &bad).unwrap();
            // 把修改时间还原，保证识别出来靠的是首尾哈希而不是时间
            let f = fs::OpenOptions::new().write(true).open(&opt).unwrap();
            f.set_modified(time).unwrap();
            drop(f);
            assert!(!plan(&t.0, &enc, Some(FP)).cache_hit, "pos={pos}");
            fs::write(&opt, &big).unwrap();
            let f = fs::OpenOptions::new().write(true).open(&opt).unwrap();
            f.set_modified(time).unwrap();
            drop(f);
            assert!(plan(&t.0, &enc, Some(FP)).cache_hit);
        }
        // 只改修改时间
        let f = fs::OpenOptions::new().write(true).open(&opt).unwrap();
        f.set_modified(SystemTime::now() + Duration::from_secs(5000))
            .unwrap();
        drop(f);
        assert!(!plan(&t.0, &enc, Some(FP)).cache_hit);
    }

    #[test]
    fn damaged_cache_file_falls_back() {
        let (t, enc) = model_dir(None);
        generate_ok(&t.0, &enc, FP, &Fake::ok(None));
        let opt = t.0.join(OPT_DIR).join(OPT_ENCODER_FILE);
        fs::write(&opt, b"x").unwrap();
        let p = plan(&t.0, &enc, Some(FP));
        assert!(!p.cache_hit && p.want_generation);
        assert_eq!(p.encoder, enc);
        // 元数据损坏同理
        fs::write(t.0.join(OPT_DIR).join(OPT_META_FILE), "{oops").unwrap();
        let p = plan(&t.0, &enc, Some(FP));
        assert!(!p.cache_hit && p.want_generation);
    }

    #[test]
    fn failed_generation_cleans_up_and_is_not_retried() {
        let (t, enc) = model_dir(Some(r#"{"model_type":"zipformer2"}"#));
        let fake = Fake::failing();
        let err = generate(&t.0, &enc, FP, &ONLINE_TRANSDUCER_TYPES, &fake).unwrap_err();
        assert!(err.contains("故意失败"), "{err}");
        assert!(!t.0.join(OPT_DIR).join(OPT_TMP_FILE).exists());
        assert!(!t.0.join(OPT_DIR).join(OPT_ENCODER_FILE).exists());
        let p = plan(&t.0, &enc, Some(FP));
        assert_eq!(p.encoder, enc);
        assert!(
            !p.cache_hit && !p.want_generation,
            "失败过的源文件不应反复重试"
        );
        assert_eq!(p.model_type.as_deref(), Some("zipformer2"));
        // 再要求生成：直接跳过，不再调用优化器
        let again = generate(&t.0, &enc, FP, &ONLINE_TRANSDUCER_TYPES, &fake).unwrap();
        assert_eq!(again, GenOutcome::UpToDate);
        assert_eq!(fake.calls.get(), 1);
        // ORT 换版本后允许重试
        assert!(plan(&t.0, &enc, Some("ort-new|len=9")).want_generation);
    }

    #[test]
    fn mark_failed_disables_a_hit_cache() {
        let (t, enc) = model_dir(None);
        generate_ok(&t.0, &enc, FP, &Fake::ok(Some("zipformer2")));
        assert!(plan(&t.0, &enc, Some(FP)).cache_hit);
        mark_failed(&t.0, &enc, FP, "sherpa 创建失败");
        let p = plan(&t.0, &enc, Some(FP));
        assert!(!p.cache_hit && !p.want_generation);
        assert_eq!(p.encoder, enc);
        assert_eq!(p.model_type.as_deref(), Some("zipformer2"));
    }

    #[test]
    fn lock_blocks_generation_until_stale() {
        let (t, enc) = model_dir(None);
        fs::create_dir_all(t.0.join(OPT_DIR)).unwrap();
        let lock = t.0.join(OPT_DIR).join(OPT_LOCK_FILE);
        fs::write(&lock, b"1").unwrap();
        assert!(!plan(&t.0, &enc, Some(FP)).want_generation);
        let fake = Fake::ok(None);
        let err = generate(&t.0, &enc, FP, &ONLINE_TRANSDUCER_TYPES, &fake).unwrap_err();
        assert!(err.contains("正在生成"), "{err}");
        assert_eq!(fake.calls.get(), 0);
        assert!(lock.exists(), "别人的锁不能被我们清掉");
        // 锁过期（生成进程被杀）后可以接管
        let f = fs::OpenOptions::new().write(true).open(&lock).unwrap();
        f.set_modified(SystemTime::now() - LOCK_STALE - Duration::from_secs(60))
            .unwrap();
        drop(f);
        assert!(plan(&t.0, &enc, Some(FP)).want_generation);
        assert_eq!(generate_ok(&t.0, &enc, FP, &fake), GenOutcome::Generated);
    }

    #[test]
    fn missing_encoder_gives_plain_plan() {
        let t = TempDir::new();
        let enc = t.0.join("encoder.int8.onnx");
        let p = plan(&t.0, &enc, Some(FP));
        assert_eq!(p.encoder, enc);
        assert!(!p.cache_hit && !p.want_generation);
    }

    #[test]
    fn unwritable_cache_dir_is_an_error_not_a_panic() {
        let (t, enc) = model_dir(None);
        // 让 `.opt` 位置被一个普通文件占住，建目录必失败
        fs::write(t.0.join(OPT_DIR), b"not a dir").unwrap();
        let fake = Fake::ok(None);
        assert!(generate(&t.0, &enc, FP, &ONLINE_TRANSDUCER_TYPES, &fake).is_err());
        assert_eq!(fake.calls.get(), 0);
    }
}
