//! `host/spec.json` 快照：UI 进程每次配置变化都写一份，宿主只认这份（方案 §2）。
//!
//! 宿主不读 `config.json`、不带 i18n、不画图标，所以热键表（已解析成修饰键位 + 虚拟键码）、
//! 托盘菜单（已本地化的文案与 16px 图标）、鼠标手势绑定、开关与提示文案都在这里。
//! 版本号 [`SPEC_VERSION`] 与单调递增的 `rev`（宿主据此去重）决定怎么读。

use crate::{SPEC_VERSION, base64};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::Arc;

/// 图标像素（RGBA8，行优先），序列化时像素存成 base64 字符串。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IconRgba {
    /// 宽（像素）。
    pub width: u32,
    /// 高（像素）。
    pub height: u32,
    /// RGBA 像素，长度应为 `width * height * 4`；共享所有权，克隆快照 / 托盘计划不复制像素。
    #[serde(with = "base64::shared_bytes")]
    pub rgba: Arc<[u8]>,
}

impl IconRgba {
    /// 像素数据长度是否与尺寸相符（尺寸非零，长度恰为 `宽 * 高 * 4`）。
    ///
    /// # 返回
    /// 相符为 `true`。
    ///
    /// ```rust
    /// use snow_host_protocol::spec::IconRgba;
    /// let ok = IconRgba { width: 2, height: 1, rgba: vec![0; 8].into() };
    /// assert!(ok.is_consistent());
    /// let bad = IconRgba { width: 2, height: 2, rgba: vec![0; 8].into() };
    /// assert!(!bad.is_consistent());
    /// ```
    pub fn is_consistent(&self) -> bool {
        let expected = (self.width as usize)
            .checked_mul(self.height as usize)
            .and_then(|n| n.checked_mul(4));
        self.width != 0 && self.height != 0 && expected == Some(self.rgba.len())
    }
}

/// 一条全局热键。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotkeySpec {
    /// 按下时转给 UI 的动作 id。
    pub id: String,
    /// 来源配置键（注册成败按它回报给设置页）。
    pub config_key: String,
    /// Win32 修饰键位掩码（Alt=1、Ctrl=2、Shift=4、Win=8）。
    pub mods: u32,
    /// Win32 虚拟键码。
    pub vk: u32,
    /// 规范化的热键文本（如 `Ctrl+Alt+S`），用于日志与错误提示。
    pub text: String,
    /// 松开时转给 UI 的动作 id（按住说话）；没有则忽略松开。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_release_id: Option<String>,
    /// 热键暂停期间是否仍保持注册（只有“暂停 / 恢复”开关本身）。
    #[serde(default)]
    pub stays_when_paused: bool,
}

/// 托盘弹出菜单的主题。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrayTheme {
    /// 深色。
    Dark,
    /// 浅色。
    Light,
    /// 跟随系统：宿主在弹菜单前读注册表 `AppsUseLightTheme`。
    System,
}

/// 托盘菜单的一项。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MenuItemSpec {
    /// 普通条目。
    Item {
        /// 显示文字（UI 已本地化）。
        label: String,
        /// 是否可点击。
        enabled: bool,
        /// 勾选状态；`None` 表示不带复选框。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        checked: Option<bool>,
        /// 16px 图标；`None` 表示无图标。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon_rgba: Option<IconRgba>,
        /// 点击后转给 UI（或宿主自己处理）的动作 id。
        action_id: String,
    },
    /// 分隔线。
    Sep,
}

/// 托盘部分。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraySection {
    /// 是否创建托盘图标（设置里可关）。
    pub enabled: bool,
    /// 常态悬停提示。
    pub tooltip: String,
    /// 热键暂停期间的悬停提示。
    pub paused_tooltip: String,
    /// 托盘图标（32x32）。
    pub icon_rgba: IconRgba,
    /// 右键菜单。
    pub menu: Vec<MenuItemSpec>,
    /// 左键单击的动作 id；`None` 表示左键弹菜单。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_left: Option<String>,
    /// 中键单击的动作 id。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_middle: Option<String>,
    /// 左键双击的动作 id。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_double: Option<String>,
    /// 弹出菜单的主题。
    pub theme: TrayTheme,
}

/// 一条鼠标手势绑定（沿用配置里的名字，宿主用 `snow-platform` 的解析函数还原）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MouseBindingSpec {
    /// 手势动作标识（`screenshot_copy` 等）。
    pub action: String,
    /// 需要按住的修饰键名（`ctrl`、`alt`、`shift`、`windows`）。
    pub modifiers: Vec<String>,
    /// 鼠标键名（`left_drag` 等，与配置里的 `mouse_button` 同词表）。
    pub button: String,
}

/// 开关。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateSpec {
    /// 前台是全屏窗口时是否停用热键。
    pub fullscreen_disable: bool,
}

/// 宿主自己要弹的提示文案（UI 已本地化；气泡 / tooltip 用）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostStrings {
    /// 热键已暂停。
    pub notice_paused: String,
    /// 热键已恢复。
    pub notice_resumed: String,
    /// 恢复热键时有注册失败。
    pub notice_resume_failed: String,
    /// 前台全屏停用热键已打开。
    pub notice_gate_on: String,
    /// 前台全屏停用热键已关闭。
    pub notice_gate_off: String,
    /// 界面进程启动失败（超时、握手不通过）；空串时宿主用内置英文兜底文案。
    #[serde(default)]
    pub notice_ui_start_failed: String,
    /// 界面进程反复异常退出、进入退避；空串时宿主用内置英文兜底文案。
    #[serde(default)]
    pub notice_ui_crash_loop: String,
}

/// 保活与登录时要做的事。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostFlags {
    /// UI 必须常驻的原因（[`crate::KEEP_ALIVE_MCP`]、[`crate::KEEP_ALIVE_OCR_HOT_START`]）；空表示无。
    #[serde(default)]
    pub keep_alive: Vec<String>,
    /// 登录时需要恢复的贴图数量；大于 0 就拉起 UI。
    #[serde(default)]
    pub pins_to_restore: u32,
    /// 下一次更新检查的到点时间（Unix 秒）；`None` 表示不检查。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_due_at: Option<u64>,
}

impl HostFlags {
    /// 登录时（以及 UI 崩溃后）是否需要宿主主动拉起 UI 常驻。
    ///
    /// MCP 监听与 OCR 预热必须有 UI 进程在场才有意义，所以这两项要宿主拉起；
    /// 其余原因（含旧版本快照里的 `dictation_hotkey`）一律忽略，不预拉也不保活。
    ///
    /// # 返回
    /// 保活原因里含 `mcp` 或 `ocr_hot_start` 为 `true`。
    ///
    /// ```rust
    /// use snow_host_protocol::KEEP_ALIVE_MCP;
    /// use snow_host_protocol::spec::HostFlags;
    /// let mcp = HostFlags { keep_alive: vec![KEEP_ALIVE_MCP.into()], ..HostFlags::default() };
    /// assert!(mcp.needs_resident_ui());
    /// let legacy = HostFlags {
    ///     keep_alive: vec!["dictation_hotkey".into()],
    ///     ..HostFlags::default()
    /// };
    /// assert!(!legacy.needs_resident_ui());
    /// ```
    pub fn needs_resident_ui(&self) -> bool {
        self.keep_alive.iter().any(|reason| {
            reason == crate::KEEP_ALIVE_MCP || reason == crate::KEEP_ALIVE_OCR_HOT_START
        })
    }
}

/// 整份快照。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostSpec {
    /// 快照格式版本，见 [`SPEC_VERSION`]。
    pub spec_version: u32,
    /// 单调递增的修订号，宿主按它去重。
    pub rev: u64,
    /// 全局热键表。
    pub hotkeys: Vec<HotkeySpec>,
    /// 托盘。
    pub tray: TraySection,
    /// 鼠标手势绑定；为空则不装钩子。
    pub mouse: Vec<MouseBindingSpec>,
    /// 开关。
    pub gate: GateSpec,
    /// 提示文案。
    pub strings: HostStrings,
    /// 保活与登录时要做的事。
    pub flags: HostFlags,
}

/// 读取快照失败的原因。
#[derive(Debug)]
pub enum SpecError {
    /// 不是合法 JSON，或字段形状不对。
    Malformed(String),
    /// 版本号缺失或不是整数。
    MissingVersion,
    /// 版本号宿主不认识（带上读到的版本）。
    UnsupportedVersion(u64),
    /// 内容自相矛盾（带说明），如图标像素长度与尺寸不符。
    Invalid(String),
}

impl fmt::Display for SpecError {
    /// 输出面向日志的错误描述。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SpecError::Malformed(e) => write!(f, "快照格式错误: {e}"),
            SpecError::MissingVersion => write!(f, "快照缺少 spec_version"),
            SpecError::UnsupportedVersion(v) => write!(f, "不认识的快照版本 {v}"),
            SpecError::Invalid(e) => write!(f, "快照内容无效: {e}"),
        }
    }
}

impl std::error::Error for SpecError {}

impl HostSpec {
    /// 检查内容自洽：版本号、图标像素长度、热键键码非零、动作 id 非空。
    ///
    /// # 返回
    /// 通过为 `Ok(())`，否则 [`SpecError::Invalid`] 或 [`SpecError::UnsupportedVersion`]。
    pub fn validate(&self) -> Result<(), SpecError> {
        if self.spec_version != SPEC_VERSION {
            return Err(SpecError::UnsupportedVersion(u64::from(self.spec_version)));
        }
        let invalid = |what: String| Err(SpecError::Invalid(what));
        if !self.tray.icon_rgba.is_consistent() {
            return invalid("托盘图标像素长度与尺寸不符".into());
        }
        for (i, item) in self.tray.menu.iter().enumerate() {
            if let MenuItemSpec::Item {
                icon_rgba,
                action_id,
                ..
            } = item
            {
                if action_id.is_empty() {
                    return invalid(format!("第 {i} 项菜单没有动作 id"));
                }
                if icon_rgba.as_ref().is_some_and(|icon| !icon.is_consistent()) {
                    return invalid(format!("第 {i} 项菜单图标像素长度与尺寸不符"));
                }
            }
        }
        for hotkey in &self.hotkeys {
            if hotkey.id.is_empty() || hotkey.vk == 0 {
                return invalid(format!("热键 {} 缺少动作 id 或键码", hotkey.text));
            }
        }
        Ok(())
    }

    /// 序列化为紧凑 JSON 文本。
    ///
    /// # 返回
    /// JSON 字符串；序列化失败（不应出现）返回 [`SpecError::Malformed`]。
    pub fn to_json(&self) -> Result<String, SpecError> {
        serde_json::to_string(self).map_err(|e| SpecError::Malformed(e.to_string()))
    }

    /// 从 JSON 文本读取快照：先看版本号，再按当前格式解析并校验。
    ///
    /// 版本不认识时不尝试解析正文（未来版本的形状可能不同），宿主据此退回内置最小菜单。
    ///
    /// # 参数
    /// - `text`：`host/spec.json` 的内容。
    ///
    /// # 返回
    /// 快照，或 [`SpecError`]。
    ///
    /// ```rust
    /// use snow_host_protocol::spec::{HostSpec, SpecError};
    /// assert!(matches!(HostSpec::from_json("not json"), Err(SpecError::Malformed(_))));
    /// assert!(matches!(
    ///     HostSpec::from_json(r#"{"spec_version": 99}"#),
    ///     Err(SpecError::UnsupportedVersion(99))
    /// ));
    /// ```
    pub fn from_json(text: &str) -> Result<Self, SpecError> {
        let probe: VersionProbe =
            serde_json::from_str(text).map_err(|e| SpecError::Malformed(e.to_string()))?;
        probe.check()?;
        let spec: HostSpec =
            serde_json::from_str(text).map_err(|e| SpecError::Malformed(e.to_string()))?;
        spec.validate()?;
        Ok(spec)
    }

    /// 从可回绕的读取器流式读取快照（语义同 [`HostSpec::from_json`]）。
    ///
    /// 不把整份文件读进一个 `String`：宿主启动时少一段几十 KB 的临时缓冲，托盘态提交（堆峰值）随之下降。
    /// 先读一遍探版本号，再回绕读一遍解析正文。
    ///
    /// # 参数
    /// - `reader`：快照内容（文件加 `BufReader` 即可）。
    ///
    /// # 返回
    /// 快照，或 [`SpecError`]（读取中途的 IO 错误按 `Malformed` 报告）。
    ///
    /// ```rust
    /// use snow_host_protocol::spec::{HostSpec, SpecError};
    /// let cursor = std::io::Cursor::new(br#"{"spec_version": 99}"#.to_vec());
    /// assert!(matches!(HostSpec::from_reader(cursor), Err(SpecError::UnsupportedVersion(99))));
    /// ```
    pub fn from_reader<R: std::io::Read + std::io::Seek>(mut reader: R) -> Result<Self, SpecError> {
        let malformed = |e: &dyn fmt::Display| SpecError::Malformed(e.to_string());
        let probe: VersionProbe =
            serde_json::from_reader(&mut reader).map_err(|e| malformed(&e))?;
        probe.check()?;
        reader
            .seek(std::io::SeekFrom::Start(0))
            .map_err(|e| malformed(&e))?;
        let spec: HostSpec = serde_json::from_reader(&mut reader).map_err(|e| malformed(&e))?;
        spec.validate()?;
        Ok(spec)
    }
}

/// 只探版本号：其余字段按“忽略”跳过，不建整棵 `Value` 树（图标 base64 文本有几十 KB，
/// 整棵树会把宿主的堆峰值抬高、托盘态提交跟着涨）。
#[derive(Deserialize)]
struct VersionProbe {
    /// 版本号字段（缺失或不是整数都按“缺少版本”处理）。
    spec_version: Option<serde_json::Value>,
}

impl VersionProbe {
    /// 版本号缺失或不被认识时返回对应错误。
    fn check(&self) -> Result<(), SpecError> {
        let version = self
            .spec_version
            .as_ref()
            .and_then(serde_json::Value::as_u64)
            .ok_or(SpecError::MissingVersion)?;
        if version != u64::from(SPEC_VERSION) {
            return Err(SpecError::UnsupportedVersion(version));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一份覆盖各种字段形态的样例快照。
    fn sample() -> HostSpec {
        let icon = |w: u32| IconRgba {
            width: w,
            height: w,
            rgba: vec![7; (w * w * 4) as usize].into(),
        };
        HostSpec {
            spec_version: SPEC_VERSION,
            rev: 5,
            hotkeys: vec![
                HotkeySpec {
                    id: "capture".into(),
                    config_key: "global_shortcuts/screenshot".into(),
                    mods: 3,
                    vk: 0x53,
                    text: "Ctrl+Alt+S".into(),
                    on_release_id: None,
                    stays_when_paused: false,
                },
                HotkeySpec {
                    id: "start_dictation".into(),
                    config_key: "dictation/hold".into(),
                    mods: 0,
                    vk: 0x78,
                    text: "F9".into(),
                    on_release_id: Some("stop_dictation".into()),
                    stays_when_paused: true,
                },
            ],
            tray: TraySection {
                enabled: true,
                tooltip: "Cisox".into(),
                paused_tooltip: "Cisox (paused)".into(),
                icon_rgba: icon(32),
                menu: vec![
                    MenuItemSpec::Item {
                        label: "截图".into(),
                        enabled: true,
                        checked: None,
                        icon_rgba: Some(icon(16)),
                        action_id: "capture".into(),
                    },
                    MenuItemSpec::Sep,
                    MenuItemSpec::Item {
                        label: "暂停热键".into(),
                        enabled: true,
                        checked: Some(true),
                        icon_rgba: None,
                        action_id: crate::ACTION_TOGGLE_GLOBAL_HOTKEYS.into(),
                    },
                ],
                on_left: Some("capture".into()),
                on_middle: None,
                on_double: None,
                theme: TrayTheme::System,
            },
            mouse: vec![MouseBindingSpec {
                action: "screenshot_copy".into(),
                modifiers: vec!["windows".into()],
                button: "left_drag".into(),
            }],
            gate: GateSpec {
                fullscreen_disable: true,
            },
            strings: HostStrings {
                notice_paused: "paused".into(),
                notice_resumed: "resumed".into(),
                notice_resume_failed: "failed".into(),
                notice_gate_on: "on".into(),
                notice_gate_off: "off".into(),
                notice_ui_start_failed: "ui failed".into(),
                notice_ui_crash_loop: "ui loop".into(),
            },
            flags: HostFlags {
                keep_alive: vec![crate::KEEP_ALIVE_MCP.into()],
                pins_to_restore: 2,
                update_due_at: Some(1_800_000_000),
            },
        }
    }

    /// 序列化再反序列化得到同一份快照；图标像素以 base64 字符串存储。
    #[test]
    fn json_round_trip() {
        let spec = sample();
        let json = spec.to_json().unwrap();
        assert!(json.contains("\"spec_version\":1"));
        assert!(json.contains("\"kind\":\"sep\""));
        assert!(json.contains("\"theme\":\"system\""));
        assert!(!json.contains("\"on_middle\""), "None 字段不输出");
        assert_eq!(HostSpec::from_json(&json).unwrap(), spec);
    }

    /// 流式读取与文本读取结果一致，版本闸门与错误分类也一致。
    #[test]
    fn reader_matches_text() {
        let spec = sample();
        let json = spec.to_json().unwrap();
        let from_reader = HostSpec::from_reader(std::io::Cursor::new(json.as_bytes())).unwrap();
        assert_eq!(from_reader, spec);
        for text in [
            r#"{"rev": 1}"#,
            r#"{"spec_version": 2, "weird": []}"#,
            "",
            "[]",
        ] {
            let by_text = HostSpec::from_json(text).map(|_| ()).unwrap_err();
            let by_reader = HostSpec::from_reader(std::io::Cursor::new(text.as_bytes()))
                .map(|_| ())
                .unwrap_err();
            assert_eq!(by_text.to_string(), by_reader.to_string(), "{text:?}");
        }
    }

    /// 缺省字段按默认值读入（旧快照缺新字段时不致命）。
    #[test]
    fn optional_fields_default() {
        let mut value: serde_json::Value =
            serde_json::from_str(&sample().to_json().unwrap()).unwrap();
        value["flags"] = serde_json::json!({});
        value["hotkeys"][0]
            .as_object_mut()
            .unwrap()
            .remove("stays_when_paused");
        let spec = HostSpec::from_json(&value.to_string()).unwrap();
        assert_eq!(spec.flags, HostFlags::default());
        assert!(!spec.hotkeys[0].stays_when_paused);
    }

    /// 版本号缺失、不认识、不是整数都被识别，且不会去解析正文。
    #[test]
    fn version_gate() {
        assert!(matches!(
            HostSpec::from_json(r#"{"rev": 1}"#),
            Err(SpecError::MissingVersion)
        ));
        assert!(matches!(
            HostSpec::from_json(r#"{"spec_version": "1"}"#),
            Err(SpecError::MissingVersion)
        ));
        assert!(matches!(
            HostSpec::from_json(r#"{"spec_version": 2, "weird": []}"#),
            Err(SpecError::UnsupportedVersion(2))
        ));
        assert!(matches!(
            HostSpec::from_json(""),
            Err(SpecError::Malformed(_))
        ));
        assert!(matches!(
            HostSpec::from_json(r#"{"spec_version": 1}"#),
            Err(SpecError::Malformed(_))
        ));
    }

    /// 内容自相矛盾会被拦下：图标像素长度不对、动作 id 为空、键码为 0。
    #[test]
    fn validation_catches_inconsistent_content() {
        let mut spec = sample();
        spec.tray.icon_rgba.rgba = vec![7; 32 * 32 * 4 - 1].into();
        assert!(matches!(spec.validate(), Err(SpecError::Invalid(_))));

        let mut spec = sample();
        if let MenuItemSpec::Item { action_id, .. } = &mut spec.tray.menu[0] {
            action_id.clear();
        }
        assert!(matches!(spec.validate(), Err(SpecError::Invalid(_))));

        let mut spec = sample();
        if let MenuItemSpec::Item { icon_rgba, .. } = &mut spec.tray.menu[0] {
            icon_rgba.as_mut().unwrap().width = 99;
        }
        assert!(matches!(spec.validate(), Err(SpecError::Invalid(_))));

        let mut spec = sample();
        spec.hotkeys[0].vk = 0;
        assert!(matches!(spec.validate(), Err(SpecError::Invalid(_))));

        let mut spec = sample();
        spec.spec_version = 9;
        assert!(matches!(
            spec.validate(),
            Err(SpecError::UnsupportedVersion(9))
        ));
        assert!(sample().validate().is_ok());
    }

    /// 登录拉起 UI 的判定：只有 MCP 与 OCR 预热算；空列表与旧快照里的 `dictation_hotkey`（已废弃）都不算。
    #[test]
    fn resident_ui_flags() {
        let flags = |reasons: &[&str]| HostFlags {
            keep_alive: reasons.iter().map(|r| r.to_string()).collect(),
            ..HostFlags::default()
        };
        assert!(!flags(&[]).needs_resident_ui());
        assert!(flags(&[crate::KEEP_ALIVE_MCP]).needs_resident_ui());
        assert!(flags(&[crate::KEEP_ALIVE_OCR_HOT_START]).needs_resident_ui());
        // 旧快照里的语音热键保活原因：忽略
        assert!(!flags(&["dictation_hotkey"]).needs_resident_ui());
        assert!(flags(&["dictation_hotkey", crate::KEEP_ALIVE_MCP]).needs_resident_ui());
    }

    /// 图标尺寸与长度校验的边界：零尺寸、溢出。
    #[test]
    fn icon_consistency_edges() {
        let zero = IconRgba {
            width: 0,
            height: 0,
            rgba: Arc::from(Vec::new()),
        };
        assert!(!zero.is_consistent());
        let huge = IconRgba {
            width: u32::MAX,
            height: u32::MAX,
            rgba: Arc::from(Vec::new()),
        };
        assert!(!huge.is_consistent());
    }
}
