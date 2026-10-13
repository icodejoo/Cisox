//! `host/spec.json` 的写入与 `rev` 维护（UI 进程写、托盘宿主读，方案 §2）。
//!
//! 写入是原子的（同目录临时文件 + 落盘 + 改名覆盖），宿主读到的要么是旧版要么是新版，不会是半份。
//! `rev` 单调递增：启动时接着文件里已有的修订号继续往上加，内容没变就不写、不涨 `rev`，
//! 所以 UI 每次启动“校验并重写快照”不会让宿主白白重新应用一遍。

use snow_host_protocol::spec::{HostFlags, HostSpec};
use snow_host_protocol::spec_file::read_spec;
use snow_platform::atomic_file::atomic_write;
use std::path::PathBuf;

/// 快照同步器。
#[derive(Debug)]
pub struct SpecSync {
    /// 快照文件路径。
    path: PathBuf,
    /// 当前文件里的修订号（文件不存在或损坏为 0）。
    rev: u64,
    /// 文件里当前的内容（用来判断新快照有没有实质变化）。
    last: Option<HostSpec>,
}

impl SpecSync {
    /// 接上已有的快照文件：能读到就沿用它的 `rev` 与内容；不存在或损坏就从 0 开始。
    ///
    /// # 参数
    /// - `path`：快照路径（`snow_data_paths::host_spec_path(数据根)`）。
    ///
    /// ```no_run
    /// use snow_shot::host_spec_sync::SpecSync;
    /// let sync = SpecSync::open(snow_data_paths::host_spec_path(std::path::Path::new("data")));
    /// assert_eq!(sync.rev(), 0);
    /// ```
    pub fn open(path: PathBuf) -> Self {
        let last = read_spec(&path).ok();
        Self {
            rev: last.as_ref().map_or(0, |spec| spec.rev),
            last,
            path,
        }
    }

    /// 当前修订号。
    pub fn rev(&self) -> u64 {
        self.rev
    }

    /// 快照是否可能已过期：文件不存在，或比任何一个“来源文件”（`config.json`、UI exe）更旧。
    ///
    /// UI 每次启动都重新生成快照要 ~235ms（i18n + 图标光栅化），不值得；配置变化时本来就即时重写，
    /// 所以启动时只需要看看有没有被绕过 UI 的改动（手工改配置、导入）或升级落下。
    ///
    /// # 参数
    /// - `sources`：快照所依赖的文件路径；读不到修改时间的忽略。
    ///
    /// ```no_run
    /// # use snow_shot::host_spec_sync::SpecSync;
    /// # fn demo(sync: &SpecSync) {
    /// let stale = sync.is_stale(&[std::path::Path::new("config.json")]);
    /// # }
    /// ```
    pub fn is_stale(&self, sources: &[&std::path::Path]) -> bool {
        let Ok(spec_time) = std::fs::metadata(&self.path).and_then(|m| m.modified()) else {
            return true;
        };
        sources.iter().any(|source| {
            std::fs::metadata(source)
                .and_then(|m| m.modified())
                .is_ok_and(|time| time > spec_time)
        })
    }

    /// 发布一份新快照：内容（忽略 `rev`）和文件里的一样就什么都不做；不一样就 `rev + 1` 原子写入。
    ///
    /// # 参数
    /// - `spec`：新快照（里面的 `rev` 会被覆盖）。
    ///
    /// # 返回
    /// 写入了新版本返回 `Some(新 rev)`；内容没变返回 `None`；写盘失败返回错误文本（状态不变，下次重试）。
    ///
    /// ```no_run
    /// # use snow_shot::host_spec_sync::SpecSync;
    /// # fn demo(mut sync: SpecSync, spec: snow_host_protocol::spec::HostSpec) {
    /// if let Ok(Some(rev)) = sync.publish(spec) {
    ///     println!("快照已更新到 {rev}");
    /// }
    /// # }
    /// ```
    pub fn publish(&mut self, mut spec: HostSpec) -> Result<Option<u64>, String> {
        if let Some(last) = &self.last {
            spec.rev = last.rev;
            if spec == *last {
                return Ok(None);
            }
        }
        spec.rev = self.rev + 1;
        let text = spec.to_json().map_err(|e| e.to_string())?;
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("创建快照目录失败: {e}"))?;
        }
        atomic_write(&self.path, text.as_bytes()).map_err(|e| format!("写入快照失败: {e}"))?;
        self.rev = spec.rev;
        self.last = Some(spec);
        Ok(Some(self.rev))
    }

    /// 只改文件里现有快照的 `flags`（登录恢复贴图数、更新检查到点时间……），不重新渲染托盘菜单。
    ///
    /// 这类字段变得勤（每次贴图增减 / 更新检查结束），而整份快照生成一次要 ~235ms（i18n + 图标光栅化），
    /// 无窗口更新检查还可能与正在运行的 UI 并存，不该去碰贴图仓储：所以只做“读现有快照 → 改标志 → 发布”。
    ///
    /// # 参数
    /// - `apply`：对 `flags` 的修改。
    ///
    /// # 返回
    /// 写入了新版本返回 `Some(新 rev)`；没有现成快照（留给完整生成）或标志没变返回 `None`；写盘失败返回错误文本。
    ///
    /// ```no_run
    /// # use snow_shot::host_spec_sync::SpecSync;
    /// # fn demo(mut sync: SpecSync) {
    /// let _ = sync.patch_flags(|flags| flags.pins_to_restore = 3);
    /// # }
    /// ```
    pub fn patch_flags(
        &mut self,
        apply: impl FnOnce(&mut HostFlags),
    ) -> Result<Option<u64>, String> {
        let Some(last) = &self.last else {
            return Ok(None);
        };
        let mut spec = last.clone();
        apply(&mut spec.flags);
        self.publish(spec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snow_host_protocol::SPEC_VERSION;
    use snow_host_protocol::spec::{GateSpec, HostStrings, IconRgba, TraySection, TrayTheme};

    /// 最小合法快照，`tooltip` 用来区分内容。
    fn spec(tooltip: &str) -> HostSpec {
        HostSpec {
            spec_version: SPEC_VERSION,
            rev: 0,
            hotkeys: vec![],
            tray: TraySection {
                enabled: true,
                tooltip: tooltip.into(),
                paused_tooltip: "paused".into(),
                icon_rgba: IconRgba {
                    width: 1,
                    height: 1,
                    rgba: vec![0; 4].into(),
                },
                menu: vec![],
                on_left: None,
                on_middle: None,
                on_double: None,
                theme: TrayTheme::System,
            },
            mouse: vec![],
            gate: GateSpec::default(),
            strings: HostStrings::default(),
            flags: HostFlags::default(),
        }
    }

    /// 创建本进程内唯一的临时快照路径（父目录由 `publish` 负责创建）。
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("snow-spec-sync-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("host").join("spec.json")
    }

    /// 首次发布创建目录与文件、rev 从 1 起；内容没变不写不涨；变了才涨。
    #[test]
    fn publish_bumps_rev_only_on_change() {
        let path = scratch("bump");
        let mut sync = SpecSync::open(path.clone());
        assert_eq!(sync.rev(), 0);
        assert_eq!(sync.publish(spec("a")).unwrap(), Some(1));
        assert_eq!(read_spec(&path).unwrap().rev, 1);
        assert_eq!(sync.publish(spec("a")).unwrap(), None, "内容没变不重写");
        assert_eq!(sync.publish(spec("b")).unwrap(), Some(2));
        assert_eq!(read_spec(&path).unwrap().tray.tooltip, "b");
        assert_eq!(sync.rev(), 2);
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }

    /// 重新打开（模拟 UI 重启）：沿用文件里的 rev 继续往上加；内容一样仍然不涨。
    #[test]
    fn reopen_continues_from_existing_rev() {
        let path = scratch("reopen");
        let mut first = SpecSync::open(path.clone());
        first.publish(spec("a")).unwrap();
        first.publish(spec("b")).unwrap();
        let mut second = SpecSync::open(path.clone());
        assert_eq!(second.rev(), 2);
        assert_eq!(second.publish(spec("b")).unwrap(), None);
        assert_eq!(second.publish(spec("c")).unwrap(), Some(3));
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }

    /// 只改标志：保持其余内容不变、rev + 1；标志没变不写；没有现成快照不凭空造（留给完整生成）。
    #[test]
    fn patch_flags_touches_only_flags() {
        let path = scratch("patch");
        let mut sync = SpecSync::open(path.clone());
        assert_eq!(
            sync.patch_flags(|flags| flags.pins_to_restore = 2).unwrap(),
            None,
            "没有基线快照"
        );
        assert!(!path.exists());
        sync.publish(spec("keep me")).unwrap();
        assert_eq!(
            sync.patch_flags(|flags| flags.pins_to_restore = 2).unwrap(),
            Some(2)
        );
        let read = read_spec(&path).unwrap();
        assert_eq!(read.flags.pins_to_restore, 2);
        assert_eq!(read.tray.tooltip, "keep me");
        assert_eq!(
            sync.patch_flags(|flags| flags.pins_to_restore = 2).unwrap(),
            None,
            "没变不写"
        );
        assert_eq!(
            sync.patch_flags(|flags| flags.update_due_at = Some(99))
                .unwrap(),
            Some(3)
        );
        let read = read_spec(&path).unwrap();
        assert_eq!(
            (read.flags.pins_to_restore, read.flags.update_due_at),
            (2, Some(99))
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }

    /// 文件损坏：从 0 开始，下一次发布覆盖掉坏文件。
    #[test]
    fn corrupt_file_is_overwritten() {
        let path = scratch("corrupt");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ broken").unwrap();
        let mut sync = SpecSync::open(path.clone());
        assert_eq!(sync.rev(), 0);
        assert_eq!(sync.publish(spec("a")).unwrap(), Some(1));
        assert_eq!(read_spec(&path).unwrap().tray.tooltip, "a");
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }

    /// 过期判定：快照不存在算过期；比来源文件旧算过期；不比它们旧不算；读不到时间的来源忽略。
    #[test]
    fn staleness_follows_file_times() {
        let path = scratch("stale");
        let dir = path.parent().unwrap().parent().unwrap().to_path_buf();
        let source = dir.join("config.json");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&source, "{}").unwrap();
        let mut sync = SpecSync::open(path.clone());
        assert!(sync.is_stale(&[source.as_path()]), "没有快照文件必然过期");
        sync.publish(spec("a")).unwrap();
        assert!(!sync.is_stale(&[source.as_path()]));
        // 把来源文件的修改时间推到快照之后
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(&source)
            .unwrap()
            .set_modified(later)
            .unwrap();
        assert!(sync.is_stale(&[source.as_path()]));
        assert!(!sync.is_stale(&[dir.join("missing.exe").as_path()]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 写盘失败（目标路径是目录）：返回错误，状态不变，不残留临时文件。
    #[test]
    fn write_failure_keeps_state() {
        let path = scratch("fail");
        std::fs::create_dir_all(&path).unwrap();
        let mut sync = SpecSync::open(path.clone());
        assert!(sync.publish(spec("a")).is_err());
        assert_eq!(sync.rev(), 0);
        let siblings: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(siblings.len(), 1, "只有 spec.json 这个目录，没有临时文件");
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }

    /// 原子写：并发读取线程反复读，任何时刻读到的要么不存在要么是完整合法的快照，从不出现半份。
    #[test]
    fn readers_never_see_a_partial_file() {
        let path = scratch("atomic");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut sync = SpecSync::open(path.clone());
        sync.publish(spec("seed")).unwrap();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reader = {
            let stop = std::sync::Arc::clone(&stop);
            let path = path.clone();
            std::thread::spawn(move || {
                let mut reads = 0u32;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    match std::fs::read_to_string(&path) {
                        Ok(text) => {
                            HostSpec::from_json(&text).expect("读到的必须是完整合法的快照");
                            reads += 1;
                        }
                        // 改名瞬间的共享冲突在 Windows 上是允许的，宿主侧的 `read_spec` 会重试
                        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {}
                        Err(e) => panic!("意外的读取错误: {e}"),
                    }
                }
                reads
            })
        };
        for i in 0..200 {
            sync.publish(spec(&format!("tooltip-{i}"))).unwrap();
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let reads = reader.join().unwrap();
        assert!(reads > 0);
        assert_eq!(sync.rev(), 201);
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }
}
