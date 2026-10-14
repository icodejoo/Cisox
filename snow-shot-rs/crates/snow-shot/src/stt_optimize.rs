//! 在后台预生成语音模型的 encoder 优化缓存：拉起 `cisox-stt --optimize <模型目录>`。
//!
//! 缓存的作用与格式见 `docs/guides/snow-stt-worker.md`「加载提速」。没有缓存时第一次听写要加载 5~10 s，
//! 命中后约 1 s，所以设置页扫描到已装的流式模型还没有缓存时就顺手生成，用户第一次按热键就能吃到。
//! 拉起方式对齐 worker 自己的 `optcache::spawn_background`：无窗口、空闲优先级、stdio 全 null、不等待子进程。
//! 失败只记 warn 日志，不影响设置页。

use crate::dictation::client::locate_stt_exe;
use crate::stt_models::{self, SttModelSpec};
use snow_stt_protocol::ModelKind;
use std::collections::HashSet;
use std::path::Path;
use std::process::{Command, Stdio};

/// worker 的预优化命令行开关；必须和 worker 端 `optcache::ARG_OPTIMIZE` 保持一致。
const ARG_OPTIMIZE: &str = "--optimize";
/// 缓存子目录名（位于模型目录下）；必须和 worker 端 `optcache.rs` 的 `OPT_DIR` 保持一致。
const OPT_DIR: &str = ".opt";
/// 缓存元数据文件名；必须和 worker 端 `optcache.rs` 的 `OPT_META_FILE` 保持一致。
const OPT_META_FILE: &str = "encoder.opt.json";
/// 生成锁文件名；必须和 worker 端 `optcache.rs` 的 `OPT_LOCK_FILE` 保持一致。
const OPT_LOCK_FILE: &str = "encoder.opt.lock";
/// Windows 进程创建标志：空闲优先级类（不和前台识别、录制抢 CPU）。
#[cfg(windows)]
const IDLE_PRIORITY_CLASS: u32 = 0x0000_0040;

/// 组装后台优化命令：参数、工作目录（worker 的 DLL 与 exe 同目录）、stdio 全 null、无窗口 + 空闲优先级。
///
/// # 参数
/// - `exe`：worker 可执行文件。
/// - `model_dir`：模型目录。
///
/// # 返回
/// 尚未启动的命令。
pub fn optimize_command(exe: &Path, model_dir: &Path) -> Command {
    let mut command = Command::new(exe);
    command
        .arg(ARG_OPTIMIZE)
        .arg(model_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(dir) = exe.parent() {
        command.current_dir(dir);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(snow_platform::CREATE_NO_WINDOW | IDLE_PRIORITY_CLASS);
    }
    command
}

/// 拉起后台优化进程，不等待它结束（父进程先退出它也继续跑）。
///
/// # 参数
/// - `exe`：worker 可执行文件。
/// - `model_dir`：模型目录。
///
/// # 返回
/// 拉起成功返回 `Ok`；创建进程失败返回原因。
pub fn spawn_optimize(exe: &Path, model_dir: &Path) -> Result<(), String> {
    optimize_command(exe, model_dir)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("{}: {e}", exe.display()))
}

/// 模型目录里是否还需要生成缓存。
///
/// 只做一次轻量的存在性检查：元数据文件在就不生成（无论 ready 还是 failed，失效与重试由 worker 自己判断，
/// 会话结束后也会补）；生成锁在说明已有进程在生成，同样不生成。
///
/// # 参数
/// - `model_dir`：模型目录。
///
/// # 返回
/// 元数据与锁都不存在返回 `true`。
///
/// ```ignore
/// if needs_generation(&model_dir) { /* 该拉起 --optimize */ }
/// ```
pub fn needs_generation(model_dir: &Path) -> bool {
    let opt = model_dir.join(OPT_DIR);
    !opt.join(OPT_META_FILE).exists() && !opt.join(OPT_LOCK_FILE).exists()
}

/// 从已安装的模型里挑出现在要拉起生成的：流式 transducer、本进程还没尝试过、目录里还没有缓存。
///
/// worker 的 `--optimize` 只处理流式 transducer（`optcache::ONLINE_TRANSDUCER_TYPES`），离线模型不拉起；
/// 判定只看清单里的 `kind`，不硬编码模型 ID。
///
/// # 参数
/// - `installed`：已安装的模型。
/// - `data_root`：应用数据根目录。
/// - `tried`：本进程已尝试过的模型 ID。
///
/// # 返回
/// 要拉起的模型，顺序同 `installed`。
pub fn pending_models<'a>(
    installed: &[&'a SttModelSpec],
    data_root: &Path,
    tried: &HashSet<String>,
) -> Vec<&'a SttModelSpec> {
    installed
        .iter()
        .copied()
        .filter(|spec| {
            spec.kind == ModelKind::OnlineTransducer
                && !tried.contains(&spec.id)
                && needs_generation(&stt_models::model_dir(spec, data_root))
        })
        .collect()
}

/// 对已安装且缺缓存的流式模型各拉起一次生成；同一模型在本进程内只尝试一次（成败都记入 `tried`）。
///
/// 设置页每次刷新安装状态都会调用，所以不命中的情况只花几次文件存在性检查。
///
/// # 参数
/// - `installed`：已安装的模型。
/// - `data_root`：应用数据根目录。
/// - `tried`：本进程已尝试过的模型 ID，会被写入本次尝试的模型。
///
/// ```ignore
/// optimize_missing(&installed, &data_root, &mut self.stt_optimize_tried);
/// ```
pub fn optimize_missing(
    installed: &[&SttModelSpec],
    data_root: &Path,
    tried: &mut HashSet<String>,
) {
    optimize_missing_with(installed, data_root, tried, |spec| {
        spawn_for_model(spec, data_root)
    });
}

/// 同 [`optimize_missing`]，拉起动作由调用方注入（测试用）。
fn optimize_missing_with(
    installed: &[&SttModelSpec],
    data_root: &Path,
    tried: &mut HashSet<String>,
    mut spawn: impl FnMut(&SttModelSpec),
) {
    for spec in pending_models(installed, data_root, tried) {
        tried.insert(spec.id.clone());
        spawn(spec);
    }
}

/// 在后台拉起某个模型的缓存生成：找不到 exe 或拉起失败只记 warn。
///
/// # 参数
/// - `spec`：模型。
/// - `data_root`：应用数据根目录。
fn spawn_for_model(spec: &SttModelSpec, data_root: &Path) {
    let model_dir = stt_models::model_dir(spec, data_root);
    let Some(exe) = locate_stt_exe() else {
        tracing::warn!(model = %spec.id, "找不到语音转文字进程，跳过 encoder 预优化缓存的生成");
        return;
    };
    match spawn_optimize(&exe, &model_dir) {
        Ok(()) => tracing::info!(model = %spec.id, "已在后台拉起 encoder 预优化缓存生成"),
        Err(e) => tracing::warn!(model = %spec.id, error = %e, "拉起 encoder 预优化缓存生成失败"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snow_stt_protocol::RecognitionMode;

    /// 清单里：流式模型全部是流式 transducer（会被预生成），离线模型全部不是，且两类都有。
    #[test]
    fn manifest_models_follow_mode() {
        let models = &stt_models::manifest().models;
        let mut streaming = 0;
        let mut offline = 0;
        for spec in models {
            let expected = spec.mode == RecognitionMode::Streaming;
            assert_eq!(
                spec.kind == ModelKind::OnlineTransducer,
                expected,
                "{}",
                spec.id
            );
            if expected {
                streaming += 1;
            } else {
                offline += 1;
            }
        }
        assert!(streaming > 0 && offline > 0, "清单应同时含流式与离线模型");
    }

    /// 命令里程序、参数（与 worker 约定的开关加模型目录）与工作目录都对（工作目录取 exe 所在目录）。
    #[test]
    fn command_has_args_and_current_dir() {
        let exe = Path::new("C:/app/cisox-stt.exe");
        let dir = Path::new("D:/data/models/stt/m");
        let command = optimize_command(exe, dir);
        assert_eq!(command.get_program(), exe.as_os_str());
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args, vec!["--optimize".as_ref(), dir.as_os_str()]);
        assert_eq!(command.get_current_dir(), Some(Path::new("C:/app")));
    }

    /// 造一个空的临时数据根。
    fn temp_root(tag: &str) -> std::path::PathBuf {
        crate::test_util::unique_temp_dir("snow-stt-opt", tag)
    }

    /// 在模型目录下造 `.opt/<file>`。
    fn touch_opt(model_dir: &Path, file: &str) {
        let opt = model_dir.join(OPT_DIR);
        std::fs::create_dir_all(&opt).expect("建 .opt");
        std::fs::write(opt.join(file), b"x").expect("写");
    }

    /// 无 `.opt` 要生成；有元数据（不论内容）不生成；只有锁也不生成；空 `.opt` 目录仍要生成。
    #[test]
    fn needs_generation_by_marker_files() {
        let root = temp_root("needs");
        let none = root.join("none");
        std::fs::create_dir_all(&none).expect("建目录");
        assert!(needs_generation(&none));
        assert!(needs_generation(&root.join("missing")));

        let empty_opt = root.join("empty");
        std::fs::create_dir_all(empty_opt.join(OPT_DIR)).expect("建 .opt");
        assert!(needs_generation(&empty_opt));

        let with_json = root.join("json");
        touch_opt(&with_json, OPT_META_FILE);
        assert!(!needs_generation(&with_json));

        let only_lock = root.join("lock");
        touch_opt(&only_lock, OPT_LOCK_FILE);
        assert!(!needs_generation(&only_lock));
    }

    /// 取清单里的一个流式、一个离线模型。
    fn pick_models() -> (&'static SttModelSpec, &'static SttModelSpec) {
        let models = &stt_models::manifest().models;
        let streaming = models
            .iter()
            .find(|m| m.kind == ModelKind::OnlineTransducer)
            .expect("清单应有流式模型");
        let offline = models
            .iter()
            .find(|m| m.kind != ModelKind::OnlineTransducer)
            .expect("清单应有离线模型");
        (streaming, offline)
    }

    /// 只挑缺缓存的流式模型：离线、已有缓存、正在生成（锁）、本进程已尝试的都不挑。
    #[test]
    fn pending_models_filters() {
        let root = temp_root("pending");
        let (streaming, offline) = pick_models();
        let installed = [streaming, offline];
        let tried = HashSet::new();
        let pending = pending_models(&installed, &root, &tried);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, streaming.id);

        let dir = stt_models::model_dir(streaming, &root);
        touch_opt(&dir, OPT_META_FILE);
        assert!(pending_models(&installed, &root, &tried).is_empty());

        std::fs::remove_dir_all(dir.join(OPT_DIR)).expect("清缓存");
        touch_opt(&dir, OPT_LOCK_FILE);
        assert!(pending_models(&installed, &root, &tried).is_empty());

        std::fs::remove_dir_all(dir.join(OPT_DIR)).expect("清锁");
        let tried = HashSet::from([streaming.id.clone()]);
        assert!(pending_models(&installed, &root, &tried).is_empty());
    }

    /// 重复刷新同一模型只拉起一次；拉起后（即使缓存没出现）不再重试。
    #[test]
    fn optimize_missing_tries_each_model_once() {
        let root = temp_root("once");
        let (streaming, offline) = pick_models();
        let installed = [streaming, offline];
        let mut tried = HashSet::new();
        let mut spawned: Vec<String> = Vec::new();
        for _ in 0..3 {
            optimize_missing_with(&installed, &root, &mut tried, |spec| {
                spawned.push(spec.id.clone());
            });
        }
        assert_eq!(spawned, vec![streaming.id.clone()]);
        assert!(tried.contains(&streaming.id));
        assert!(!tried.contains(&offline.id));
    }

    /// 真实 worker 联调（手动跑，`--ignored`）：拉起后轮询缓存元数据直到 `state=ready`。
    ///
    /// 环境变量：`SNOW_STT_EXE`（worker）、`SNOW_STT_TEST_MODEL_DIR`（还没有 `.opt` 的流式模型目录）。
    #[test]
    #[ignore = "需要真实 worker 与已安装模型，手动运行"]
    fn spawn_optimize_generates_ready_cache() {
        let (Some(exe), Some(dir)) = (
            std::env::var_os("SNOW_STT_EXE"),
            std::env::var_os("SNOW_STT_TEST_MODEL_DIR"),
        ) else {
            return;
        };
        let (exe, dir) = (std::path::PathBuf::from(exe), std::path::PathBuf::from(dir));
        spawn_optimize(&exe, &dir).expect("拉起");
        let meta = dir.join(OPT_DIR).join(OPT_META_FILE);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        while std::time::Instant::now() < deadline {
            let text = std::fs::read_to_string(&meta).unwrap_or_default();
            if text.contains("\"ready\"") {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        panic!("120 s 内没有生成 state=ready 的缓存元数据");
    }

    /// exe 不存在时返回带路径的错误，不 panic。
    #[test]
    fn spawn_missing_exe_returns_error() {
        let exe = Path::new("Z:/definitely/not/here/cisox-stt.exe");
        let err = spawn_optimize(exe, Path::new("D:/m")).unwrap_err();
        assert!(err.contains("cisox-stt.exe"), "{err}");
    }
}
