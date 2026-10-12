//! 无窗口任务（`--headless <任务>`）：不初始化 GPUI，做完就退（方案 §2、§8 阶段 3）。
//!
//! 托盘宿主在 UI 不在时用它做三件小事，免得为此拉起完整 UI：
//! - `emit-spec`：按当前配置生成 `host/spec.json`（宿主起来时快照缺失 / 损坏）；
//! - `update-check`：到点的自动更新检查，查完即退，有新版本把提示文案写到标准输出第一行（宿主放进托盘悬停提示）；
//! - `set-config`：改一项配置（宿主切换“前台全屏停用热键”时 UI 不在）。UI 仍是配置唯一写者，
//!   宿主保证同一时刻不会与别的 UI 会话同时写配置。

use crate::app_runtime::{locale_of, open_shared_config, tray_group_labels_for};
use crate::host_spec::{HostSpecInputs, host_spec_from_document};
use crate::host_spec_sync::SpecSync;
use crate::net_settings;
use crate::pinned_shared::PinShared;
use snow_data_paths::host_spec_path;
use snow_host_protocol::launch::HeadlessJob;
use std::path::Path;
use std::rc::Rc;

/// 生成宿主快照并写入 `host/spec.json`（`emit-spec` 的实现，也是 `set-config` 的收尾）。
///
/// 贴图分组、登录要恢复的贴图数从贴图仓储读出，更新检查到点时间从配置与上次检查时间算出，
/// 所以无窗口生成的快照与运行中的 UI 生成的一致（内容相同就不重写、`rev` 不涨）。
///
/// # 参数
/// - `data_root`：数据根目录。
///
/// # 返回
/// 写入了新版本返回 `Some(新 rev)`；内容与已有快照一致返回 `None`；失败返回错误文本。
pub fn emit_spec(data_root: &Path) -> Result<Option<u64>, String> {
    let config = open_shared_config(data_root);
    let pins = PinShared::open(data_root, Rc::clone(&config), Box::new(|_| {}));
    let config = config.borrow();
    let document = config.document();
    let locale = locale_of(document);
    let groups = tray_group_labels_for(&pins, locale);
    let spec = host_spec_from_document(
        document,
        &HostSpecInputs {
            locale,
            hotkeys_paused: false,
            groups: &groups,
            active_group: &pins.active_group_id(),
            pins_to_restore: pins.restorable_count(),
            update_due_at: net_settings::update_due_at(
                document,
                data_root,
                net_settings::unix_now(),
            ),
            rev: 0,
        },
    )?;
    SpecSync::open(host_spec_path(data_root)).publish(spec)
}

/// 到点的自动更新检查（`update-check` 的实现）：按策略 / 间隔 / 地址判断，满足才真查；
/// 先记检查时间（失败也按间隔节流），查完把快照里的到点时间往后推。
///
/// 可能与正在运行的 UI 并存，所以只改快照里的标志（[`SpecSync::patch_flags`]），不重新渲染菜单、不碰贴图仓储；
/// 没有现成快照才退回完整生成。
///
/// # 参数
/// - `data_root`：数据根目录。
///
/// # 返回
/// 发现新版本时返回已本地化的提示文案；没到点、没新版本或检查失败返回 `None`；快照更新失败返回错误文本。
pub fn update_check(data_root: &Path) -> Result<Option<String>, String> {
    let config = open_shared_config(data_root);
    let now = net_settings::unix_now();
    let (url, locale) = {
        let store = config.borrow();
        let document = store.document();
        match net_settings::auto_check_target(document, data_root, now) {
            Some(url) => (url, locale_of(document)),
            None => {
                tracing::info!("没有到点的自动更新检查");
                return Ok(None);
            }
        }
    };
    net_settings::write_last_check(data_root, now);
    let outcome = net_settings::run_update_check(&url, net_settings::APP_VERSION);
    tracing::info!(?outcome, "无窗口更新检查结束");
    let notice = net_settings::auto_check_notice(&outcome, locale);
    // 到点时间往后推：宿主读到新快照才不会对同一个到点时间再拉起一次
    let due = {
        let store = config.borrow();
        net_settings::update_due_at(store.document(), data_root, net_settings::unix_now())
    };
    let patched =
        SpecSync::open(host_spec_path(data_root)).patch_flags(|flags| flags.update_due_at = due)?;
    if patched.is_none() && !host_spec_path(data_root).is_file() {
        emit_spec(data_root)?;
    }
    Ok(notice)
}

/// 改一项配置并落盘（`set-config` 的实现），再重新生成快照（闸门等开关就在快照里）。
///
/// # 参数
/// - `data_root`：数据根目录。
/// - `key`：配置键。
/// - `value`：新值。
///
/// # 返回
/// 成功为 `Ok(())`；配置键 / 值被拒绝或落盘失败返回错误文本。
pub fn set_config(data_root: &Path, key: &str, value: serde_json::Value) -> Result<(), String> {
    let config = open_shared_config(data_root);
    {
        let mut store = config.borrow_mut();
        store.set_value(key, value).map_err(|e| e.to_string())?;
        store.flush().map_err(|e| e.to_string())?;
    }
    drop(config);
    emit_spec(data_root).map(|_| ())
}

/// 执行一个无窗口任务。
///
/// # 参数
/// - `data_root`：数据根目录。
/// - `job`：任务。
///
/// # 返回
/// 要写到标准输出的提示文案（只有更新检查发现新版本时有）；失败返回错误文本。
pub fn run(data_root: &Path, job: &HeadlessJob) -> Result<Option<String>, String> {
    match job {
        HeadlessJob::EmitSpec => emit_spec(data_root).map(|rev| {
            tracing::info!(?rev, "宿主快照已生成");
            None
        }),
        HeadlessJob::UpdateCheck => update_check(data_root),
        HeadlessJob::SetConfig { key, value } => {
            set_config(data_root, key, value.clone())?;
            tracing::info!(%key, "配置已写入");
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::unique_temp_dir;
    use serde_json::json;
    use snow_host_protocol::spec_file::read_spec;
    use snow_host_protocol::{
        CONFIG_KEY_DISABLE_ON_FULLSCREEN, KEEP_ALIVE_DICTATION_HOTKEY, KEEP_ALIVE_MCP,
    };

    /// 在数据根里存一张贴图记录（登录恢复贴图的来源）。
    fn store_one_pin(root: &Path) {
        use crate::pinned_model::PinGeometry;
        use crate::screenshot_output::encode_png;
        use snow_ui::shell::geometry::PhysicalRect;
        let config = open_shared_config(root);
        let pins = PinShared::open(root, config, Box::new(|_| {}));
        let id = pins.new_id().unwrap();
        let rgba = vec![200u8; 4 * 4 * 4];
        let png = encode_png(4, 4, &rgba).unwrap();
        let geometry = PinGeometry::new(PhysicalRect::new(0, 0, 4, 4), 1.0, 1.0, true);
        pins.persist(&id, &geometry, 1, png.len() as u64, Some(png))
            .unwrap();
    }

    /// 无窗口生成快照：空数据根生成默认快照写入 `host/spec.json`，rev 为 1；
    /// 再生成一次内容不变，不重写、rev 不涨；读回的快照合法且带默认热键与托盘菜单。
    #[test]
    fn emit_spec_writes_valid_snapshot_once() {
        let root = unique_temp_dir("snow-emit-spec", "once");
        assert_eq!(emit_spec(&root).unwrap(), Some(1));
        assert_eq!(emit_spec(&root).unwrap(), None);
        let spec = read_spec(&host_spec_path(&root)).unwrap();
        assert_eq!(spec.rev, 1);
        assert!(spec.tray.enabled);
        assert!(!spec.hotkeys.is_empty());
        assert!(!spec.tray.menu.is_empty());
        assert_eq!(spec.flags.pins_to_restore, 0);
        assert!(spec.flags.keep_alive.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 无窗口生成的快照带着贴图仓储里的登录恢复数：有一张贴图就是 1，宿主据此在登录时拉起 UI。
    #[test]
    fn emit_spec_counts_pins_to_restore() {
        let root = unique_temp_dir("snow-emit-spec", "pins");
        store_one_pin(&root);
        emit_spec(&root).unwrap();
        let spec = read_spec(&host_spec_path(&root)).unwrap();
        assert_eq!(spec.flags.pins_to_restore, 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 无窗口改配置：写进 `config.json`、快照里的闸门同步变化、rev 涨一次；再写同值不涨。
    #[test]
    fn set_config_persists_and_refreshes_spec() {
        let root = unique_temp_dir("snow-set-config", "gate");
        emit_spec(&root).unwrap();
        assert!(
            !read_spec(&host_spec_path(&root))
                .unwrap()
                .gate
                .fullscreen_disable
        );
        run(
            &root,
            &HeadlessJob::SetConfig {
                key: CONFIG_KEY_DISABLE_ON_FULLSCREEN.into(),
                value: json!(true),
            },
        )
        .unwrap();
        let spec = read_spec(&host_spec_path(&root)).unwrap();
        assert!(spec.gate.fullscreen_disable);
        assert_eq!(spec.rev, 2);
        // 配置真的落了盘（另开一份读）
        let reread = open_shared_config(&root);
        assert!(
            reread
                .borrow()
                .document()
                .bool_or(CONFIG_KEY_DISABLE_ON_FULLSCREEN, false)
        );
        drop(reread);
        set_config(&root, CONFIG_KEY_DISABLE_ON_FULLSCREEN, json!(true)).unwrap();
        assert_eq!(
            read_spec(&host_spec_path(&root)).unwrap().rev,
            2,
            "值没变不重写"
        );
        set_config(&root, CONFIG_KEY_DISABLE_ON_FULLSCREEN, json!(false)).unwrap();
        assert!(
            !read_spec(&host_spec_path(&root))
                .unwrap()
                .gate
                .fullscreen_disable
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 无窗口改配置遇到被拒绝的值（类型不对）：返回错误，不留半截写入，也不动快照。
    #[test]
    fn set_config_rejects_bad_value() {
        let root = unique_temp_dir("snow-set-config", "bad");
        emit_spec(&root).unwrap();
        let before = read_spec(&host_spec_path(&root)).unwrap();
        let result = set_config(&root, CONFIG_KEY_DISABLE_ON_FULLSCREEN, json!("not a bool"));
        assert!(result.is_err());
        assert_eq!(read_spec(&host_spec_path(&root)).unwrap(), before);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 更新检查没到点（默认配置没地址 / 手动）：直接返回，不联网、不写检查时间、不动快照。
    #[test]
    fn update_check_without_due_does_nothing() {
        let root = unique_temp_dir("snow-update-check", "idle");
        emit_spec(&root).unwrap();
        let before = read_spec(&host_spec_path(&root)).unwrap();
        assert_eq!(update_check(&root).unwrap(), None);
        assert!(!net_settings::last_check_path(&root).exists());
        assert_eq!(read_spec(&host_spec_path(&root)).unwrap(), before);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 快照里的保活原因随配置：开 MCP 后重新生成就带上 `mcp`；语音热键绑定带 `dictation_hotkey`。
    #[test]
    fn emitted_spec_carries_keep_alive_reasons() {
        let root = unique_temp_dir("snow-emit-spec", "keep");
        set_config(&root, crate::mcp_host::KEY_ENABLED, json!(true)).unwrap();
        set_config(
            &root,
            crate::app_runtime::DICTATION_TOGGLE_HOTKEY_CONFIG_KEY,
            json!(["Ctrl+Alt+D"]),
        )
        .unwrap();
        let spec = read_spec(&host_spec_path(&root)).unwrap();
        assert!(spec.flags.keep_alive.contains(&KEEP_ALIVE_MCP.to_string()));
        assert!(
            spec.flags
                .keep_alive
                .contains(&KEEP_ALIVE_DICTATION_HOTKEY.to_string())
        );
        assert!(spec.flags.needs_resident_ui());
        let _ = std::fs::remove_dir_all(&root);
    }
}
