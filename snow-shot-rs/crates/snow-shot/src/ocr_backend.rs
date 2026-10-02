//! OCR 后端抽象：把“用哪种引擎识别”从调用方里分离出来。
//!
//! 本地模型（`snow-ocr-process`）全平台可用；系统原生 OCR 目前只有 Windows（`Windows.Media.Ocr`）实现，
//! 其它平台与系统 OCR 不可用（缺语言包、接口失败）时，选了它会回落到本地模型并带上对应提示。
//! 本模块不接触任何密钥，也不会把配置内容写进日志。

use crate::ocr_client::OcrError;
use crate::ocr_service::{OcrRequestConfig, OcrResult, OcrService, OcrTextBox};
use image::{RgbaImage, imageops};
use serde_json::Value;
use snow_config::document::ConfigDocument;
use snow_config::extensions::{
    KEY_OCR_BACKEND, OCR_BACKEND_LOCAL_MODEL, OCR_BACKEND_SYSTEM, default_ocr_backend,
};
use snow_i18n::I18n;
use snow_platform::win_ocr::{self, WinOcrError, WinOcrLine, WinOcrStatus};
use snow_ui::shell::geometry::PhysicalRect;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

/// 界面语料的回退语言。
const FALLBACK_LOCALE: &str = "en-US";
/// 界面语料支持的语言（与 `snow-i18n/locales` 下的目录一致）。
const LOCALE_EN_US: &str = "en-US";
/// 简体中文语料。
const LOCALE_ZH_CN: &str = "zh-CN";
/// 繁体中文语料。
const LOCALE_ZH_TW: &str = "zh-TW";
/// 系统 OCR 可用性探测结果的缓存时长（设置页每帧都会查询，语言包安装后几秒内即可感知）。
const SYSTEM_PROBE_TTL: Duration = Duration::from_secs(5);

/// OCR 后端种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OcrBackend {
    /// 系统原生 OCR（目前仅 Windows 实现）。
    System,
    /// 本地模型（`snow-ocr-process`）。
    LocalModel,
}

impl OcrBackend {
    /// 解析配置取值。
    ///
    /// # 参数
    /// - `value`：配置里的字符串，如 `system`、`local-model`。
    ///
    /// # 返回
    /// 对应后端；未知取值返回 `None`。
    ///
    /// # 示例
    /// ```ignore
    /// assert_eq!(OcrBackend::from_config_value("system"), Some(OcrBackend::System));
    /// assert_eq!(OcrBackend::from_config_value("x"), None);
    /// ```
    pub fn from_config_value(value: &str) -> Option<Self> {
        match value {
            OCR_BACKEND_SYSTEM => Some(Self::System),
            OCR_BACKEND_LOCAL_MODEL => Some(Self::LocalModel),
            _ => None,
        }
    }

    /// 写回配置用的取值。
    pub fn config_value(self) -> &'static str {
        match self {
            Self::System => OCR_BACKEND_SYSTEM,
            Self::LocalModel => OCR_BACKEND_LOCAL_MODEL,
        }
    }

    /// 从配置文档读取当前选择；值缺失或非法时用平台默认。
    ///
    /// # 参数
    /// - `document`：配置文档。
    ///
    /// # 示例
    /// ```ignore
    /// let backend = OcrBackend::from_document(store.document());
    /// ```
    pub fn from_document(document: &ConfigDocument) -> Self {
        document
            .value(KEY_OCR_BACKEND)
            .as_str()
            .and_then(Self::from_config_value)
            .or_else(|| Self::from_config_value(default_ocr_backend()))
            .unwrap_or(Self::LocalModel)
    }

    /// 按界面语言取显示名。
    ///
    /// # 参数
    /// - `locale`：`en-US` / `zh-CN` / `zh-TW`，其它值回退英文。
    pub fn label(self, locale: &str) -> String {
        let i18n = i18n_for(locale);
        match self {
            Self::System => i18n.tr("ocr-backend-system"),
            Self::LocalModel => i18n.tr("ocr-backend-local-model"),
        }
    }
}

/// 一次识别的输入图像。
#[derive(Debug, Clone, Copy)]
pub struct OcrInput<'a> {
    /// 宽（像素）。
    pub width: u32,
    /// 高（像素）。
    pub height: u32,
    /// 紧凑 RGBA 像素，长度须为 `宽 * 高 * 4`。
    pub rgba: &'a [u8],
}

/// 后端当前是否可用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OcrAvailability {
    /// 可以直接使用。
    Ready,
    /// 当前平台没有该后端的实现。
    NotImplemented,
    /// 系统没有匹配用户语言的 OCR 语言包。
    NoLanguagePack,
    /// 系统接口调用失败（细节见日志）。
    EngineFailed,
}

/// OCR 引擎：把一张 RGBA 图识别成文本块。
pub trait OcrEngine: Send + Sync {
    /// 引擎对应的后端种类。
    fn backend(&self) -> OcrBackend;

    /// 当前是否可用（不做耗时探测）。
    fn availability(&self) -> OcrAvailability;

    /// 识别一张图（阻塞，须在后台线程调用）。
    ///
    /// # 参数
    /// - `input`：待识别图像。
    ///
    /// # 返回
    /// 识别结果；失败返回 [`OcrError`]，不会编造文本。
    ///
    /// # 示例
    /// ```ignore
    /// let result = engine.recognize(&OcrInput { width, height, rgba: &pixels })?;
    /// ```
    fn recognize(&self, input: &OcrInput<'_>) -> Result<OcrResult, OcrError>;
}

/// 本地模型引擎：包装现有 [`OcrService`]，行为与改造前一致。
pub struct LocalModelOcr {
    /// 底层服务（worker 复用与空闲退出由它管理）。
    service: Arc<OcrService>,
    /// 本次识别的配置快照。
    config: OcrRequestConfig,
}

impl LocalModelOcr {
    /// 创建本地模型引擎。
    ///
    /// # 参数
    /// - `service`：共享的 OCR 服务。
    /// - `config`：识别配置快照。
    pub fn new(service: Arc<OcrService>, config: OcrRequestConfig) -> Self {
        Self { service, config }
    }
}

impl OcrEngine for LocalModelOcr {
    /// 本地模型后端。
    fn backend(&self) -> OcrBackend {
        OcrBackend::LocalModel
    }

    /// 始终视为可用；资产缺失在识别时以 [`OcrError::Unavailable`] 报告（保持原有下载引导流程）。
    fn availability(&self) -> OcrAvailability {
        OcrAvailability::Ready
    }

    /// 转交 [`OcrService::recognize_rgba`]。
    fn recognize(&self, input: &OcrInput<'_>) -> Result<OcrResult, OcrError> {
        self.service
            .recognize_rgba(&self.config, input.width, input.height, input.rgba)
    }
}

/// 可缓存一段时间的探测结果。
struct TtlCache<T> {
    /// 上次探测的时刻与结果。
    slot: Option<(Instant, T)>,
}

impl<T: Copy> TtlCache<T> {
    /// 缓存未过期时返回旧值，否则调用 `probe` 刷新。
    ///
    /// # 参数
    /// - `now`：当前时刻。
    /// - `ttl`：有效时长。
    /// - `probe`：重新探测的函数。
    fn get_or_probe(&mut self, now: Instant, ttl: Duration, probe: impl FnOnce() -> T) -> T {
        if let Some((at, value)) = self.slot
            && now.saturating_duration_since(at) < ttl
        {
            return value;
        }
        let value = probe();
        self.slot = Some((now, value));
        value
    }
}

/// 系统 OCR 探测缓存（进程内唯一）。
static SYSTEM_PROBE: Mutex<TtlCache<OcrAvailability>> = Mutex::new(TtlCache { slot: None });

/// 把平台探测结果换成可用性（失败细节写日志，不进枚举）。
///
/// # 参数
/// - `status`：`snow-platform` 的探测结果。
fn availability_from_status(status: &WinOcrStatus) -> OcrAvailability {
    match status {
        WinOcrStatus::Ready { .. } => OcrAvailability::Ready,
        WinOcrStatus::NoLanguagePack => OcrAvailability::NoLanguagePack,
        WinOcrStatus::UnsupportedPlatform => OcrAvailability::NotImplemented,
        WinOcrStatus::EngineFailed(why) => {
            tracing::warn!(reason = %why, "系统 OCR 探测失败");
            OcrAvailability::EngineFailed
        }
    }
}

/// 把平台识别错误换成 [`OcrError`]：尺寸问题归为 `InvalidImage`，其余为 `Failed`。
fn map_system_error(error: WinOcrError) -> OcrError {
    match error {
        WinOcrError::InvalidImage(why) => OcrError::InvalidImage(why),
        other => OcrError::Failed(other.to_string()),
    }
}

/// 把系统返回的行转成原图坐标下的文本块（按缩放比还原并夹到图内，空白行丢弃，置信度为 `None`）。
///
/// # 参数
/// - `lines`：系统返回的行（提交图像坐标）。
/// - `scale`：`(x 还原比, y 还原比)`，未缩放为 `(1.0, 1.0)`。
/// - `bounds`：原图尺寸。
fn system_lines_to_boxes(lines: &[WinOcrLine], scale: (f32, f32), bounds: (u32, u32)) -> Vec<OcrTextBox> {
    let (max_x, max_y) = (bounds.0 as f32, bounds.1 as f32);
    lines
        .iter()
        .filter(|line| !line.text.trim().is_empty())
        .map(|line| {
            let [x, y, w, h] = line.rect;
            let left = (x * scale.0).clamp(0.0, max_x);
            let right = ((x + w) * scale.0).clamp(0.0, max_x);
            let top = (y * scale.1).clamp(0.0, max_y);
            let bottom = ((y + h) * scale.1).clamp(0.0, max_y);
            let (ix, iy) = (left.floor() as i32, top.floor() as i32);
            OcrTextBox {
                rect: PhysicalRect::new(ix, iy, (right.ceil() as i32 - ix).max(0), (bottom.ceil() as i32 - iy).max(0)),
                text: line.text.clone(),
                confidence: None,
            }
        })
        .collect()
}

/// 系统原生 OCR：Windows 走 `Windows.Media.Ocr`，其它平台不可用。
///
/// 没有置信度（结果里为 `None`）；识别语言取用户档语言。
pub struct SystemOcr;

impl OcrEngine for SystemOcr {
    /// 系统后端。
    fn backend(&self) -> OcrBackend {
        OcrBackend::System
    }

    /// 探测系统 OCR 是否可用（结果缓存数秒，调用很便宜）。
    fn availability(&self) -> OcrAvailability {
        SYSTEM_PROBE
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_or_probe(Instant::now(), SYSTEM_PROBE_TTL, || availability_from_status(&win_ocr::probe()))
    }

    /// 经系统 OCR 识别；图像边长超过系统上限时先等比缩小，结果坐标还原到原图。
    fn recognize(&self, input: &OcrInput<'_>) -> Result<OcrResult, OcrError> {
        let started = Instant::now();
        let expected = (input.width as usize).checked_mul(input.height as usize).and_then(|p| p.checked_mul(4));
        if input.width == 0 || input.height == 0 || expected != Some(input.rgba.len()) {
            return Err(OcrError::InvalidImage(format!(
                "尺寸 {}x{} 与像素长度 {} 不符",
                input.width,
                input.height,
                input.rgba.len()
            )));
        }
        let fitted = win_ocr::max_image_dimension().and_then(|max| win_ocr::fit_dimension(input.width, input.height, max));
        let output = match fitted {
            None => win_ocr::recognize(input.width, input.height, input.rgba, None),
            Some((sw, sh)) => {
                let source = RgbaImage::from_raw(input.width, input.height, input.rgba.to_vec())
                    .ok_or_else(|| OcrError::InvalidImage("无法构造缩放源图".to_string()))?;
                let small = imageops::resize(&source, sw, sh, imageops::FilterType::Triangle);
                win_ocr::recognize(sw, sh, small.as_raw(), None)
            }
        }
        .map_err(map_system_error)?;
        let scale = fitted.map_or((1.0, 1.0), |(sw, sh)| (input.width as f32 / sw as f32, input.height as f32 / sh as f32));
        let boxes = system_lines_to_boxes(&output.lines, scale, (input.width, input.height));
        let full_text = boxes.iter().map(|b| b.text.as_str()).collect::<Vec<_>>().join("\n");
        Ok(OcrResult { boxes, full_text, elapsed_ms: started.elapsed().as_millis() as u64 })
    }
}

/// 选择后端时附带的用户提示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OcrNotice {
    /// 选了系统 OCR，但当前平台没有实现，已改用本地模型。
    SystemNotImplemented,
    /// 选了系统 OCR，但缺少匹配用户语言的 OCR 语言包，已改用本地模型。
    SystemNoLanguagePack,
    /// 选了系统 OCR，但系统接口调用失败，已改用本地模型。
    SystemEngineFailed,
}

impl OcrNotice {
    /// 按界面语言取提示文案。
    ///
    /// # 参数
    /// - `locale`：`en-US` / `zh-CN` / `zh-TW`，其它值回退英文。
    ///
    /// # 示例
    /// ```ignore
    /// let text = OcrNotice::SystemNoLanguagePack.message("zh-CN");
    /// ```
    pub fn message(self, locale: &str) -> String {
        let id = match self {
            Self::SystemNotImplemented => "ocr-backend-system-not-implemented",
            Self::SystemNoLanguagePack => "ocr-backend-system-no-language-pack",
            Self::SystemEngineFailed => "ocr-backend-system-engine-failed",
        };
        i18n_for(locale).tr(id)
    }

    /// 由系统引擎的可用性得到提示；可用时为 `None`。
    ///
    /// # 参数
    /// - `availability`：系统引擎当前可用性。
    pub fn from_availability(availability: OcrAvailability) -> Option<Self> {
        match availability {
            OcrAvailability::Ready => None,
            OcrAvailability::NotImplemented => Some(Self::SystemNotImplemented),
            OcrAvailability::NoLanguagePack => Some(Self::SystemNoLanguagePack),
            OcrAvailability::EngineFailed => Some(Self::SystemEngineFailed),
        }
    }

    /// 配置里选了 `value` 时应展示的提示（设置页用）。
    ///
    /// # 参数
    /// - `value`：`text_recognition/backend` 的当前值。
    ///
    /// # 返回
    /// 选了不可用的系统后端时返回提示，否则 `None`。
    pub fn for_config_value(value: &Value) -> Option<Self> {
        let backend = value.as_str().and_then(OcrBackend::from_config_value)?;
        notice_if_unavailable(backend, &SystemOcr)
    }
}

/// 一次后端选择的结果。
pub struct OcrSelection {
    /// 用户在配置里选的后端。
    pub requested: OcrBackend,
    /// 实际使用的后端（可能因回落与 `requested` 不同）。
    pub effective: OcrBackend,
    /// 回落时的提示；无回落为 `None`。
    pub notice: Option<OcrNotice>,
    /// 实际使用的引擎。
    pub engine: Arc<dyn OcrEngine>,
}

impl std::fmt::Debug for OcrSelection {
    /// 只输出后端种类与提示，不展开引擎内部。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OcrSelection")
            .field("requested", &self.requested)
            .field("effective", &self.effective)
            .field("notice", &self.notice)
            .finish()
    }
}

/// 请求系统后端但其不可用时的提示（按不可用原因区分）。
fn notice_if_unavailable(requested: OcrBackend, system: &dyn OcrEngine) -> Option<OcrNotice> {
    if requested == OcrBackend::System { OcrNotice::from_availability(system.availability()) } else { None }
}

/// 由请求的后端与两个候选引擎决定实际引擎：系统不可用时回落本地模型并给出提示。
///
/// 永远不会回落到任何联网后端（P0 也没有）。
///
/// # 参数
/// - `requested`：配置选择的后端。
/// - `local`：本地模型引擎。
/// - `system`：系统引擎。
///
/// # 示例
/// ```ignore
/// let sel = select_engine(OcrBackend::System, local, Arc::new(SystemOcr));
/// assert_eq!(sel.effective, OcrBackend::LocalModel);
/// assert!(sel.notice.is_some());
/// ```
pub fn select_engine(
    requested: OcrBackend,
    local: Arc<dyn OcrEngine>,
    system: Arc<dyn OcrEngine>,
) -> OcrSelection {
    let notice = notice_if_unavailable(requested, system.as_ref());
    let engine = match (requested, notice) {
        (OcrBackend::System, None) => system,
        _ => local,
    };
    OcrSelection {
        requested,
        effective: engine.backend(),
        notice,
        engine,
    }
}

/// 按配置文档装配本次识别用的引擎。
///
/// # 参数
/// - `document`：配置文档。
/// - `service`：共享的 OCR 服务。
///
/// # 示例
/// ```ignore
/// let selection = select_from_document(store.document(), Arc::clone(&state.ocr));
/// let result = selection.engine.recognize(&input)?;
/// ```
pub fn select_from_document(document: &ConfigDocument, service: Arc<OcrService>) -> OcrSelection {
    let local = Arc::new(LocalModelOcr::new(
        service,
        OcrRequestConfig::from_document(document),
    ));
    select_engine(
        OcrBackend::from_document(document),
        local,
        Arc::new(SystemOcr),
    )
}

/// 取某界面语言的语料（进程内缓存，构建失败时返回 `None`）。
fn bundle(locale: &str) -> Option<&'static I18n> {
    static EN_US: OnceLock<Option<I18n>> = OnceLock::new();
    static ZH_CN: OnceLock<Option<I18n>> = OnceLock::new();
    static ZH_TW: OnceLock<Option<I18n>> = OnceLock::new();
    let (cell, name) = match locale {
        LOCALE_ZH_CN => (&ZH_CN, LOCALE_ZH_CN),
        LOCALE_ZH_TW => (&ZH_TW, LOCALE_ZH_TW),
        _ => (&EN_US, LOCALE_EN_US),
    };
    cell.get_or_init(|| I18n::embedded(name, FALLBACK_LOCALE, snow_app_core::PRODUCT_NAME).ok())
        .as_ref()
}

/// 语料句柄：语料构建失败时用空语料，`tr` 会返回降级文案而不会 panic。
pub(crate) fn i18n_for(locale: &str) -> &'static I18n {
    static EMPTY: OnceLock<I18n> = OnceLock::new();
    bundle(locale).unwrap_or_else(|| {
        EMPTY.get_or_init(|| {
            I18n::from_resources(
                FALLBACK_LOCALE,
                FALLBACK_LOCALE,
                snow_app_core::PRODUCT_NAME,
                &[],
            )
            .expect("空语料总能构建")
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocr_assets::OcrUnavailable;
    use serde_json::json;
    use std::path::PathBuf;
    use std::time::Duration;

    /// 假引擎：可配置可用性，记录识别次数。
    struct FakeEngine {
        /// 对应后端。
        backend: OcrBackend,
        /// 可用性。
        availability: OcrAvailability,
    }

    impl OcrEngine for FakeEngine {
        /// 返回配置的后端。
        fn backend(&self) -> OcrBackend {
            self.backend
        }
        /// 返回配置的可用性。
        fn availability(&self) -> OcrAvailability {
            self.availability
        }
        /// 固定返回一行文本。
        fn recognize(&self, _input: &OcrInput<'_>) -> Result<OcrResult, OcrError> {
            Ok(OcrResult {
                full_text: format!("{:?}", self.backend),
                ..OcrResult::default()
            })
        }
    }

    /// 构造假引擎。
    fn fake(backend: OcrBackend, availability: OcrAvailability) -> Arc<dyn OcrEngine> {
        Arc::new(FakeEngine {
            backend,
            availability,
        })
    }

    /// 配置值与枚举互转，未知值为 None。
    #[test]
    fn config_value_round_trip() {
        for backend in [OcrBackend::System, OcrBackend::LocalModel] {
            assert_eq!(
                OcrBackend::from_config_value(backend.config_value()),
                Some(backend)
            );
        }
        assert_eq!(OcrBackend::from_config_value("remote-api"), None);
        assert_eq!(OcrBackend::from_config_value(""), None);
    }

    /// 从文档读取：新文档用平台默认，显式写入的值被尊重。
    #[test]
    fn backend_from_document() {
        let mut doc = ConfigDocument::from_bytes(None);
        assert_eq!(
            OcrBackend::from_document(&doc).config_value(),
            default_ocr_backend()
        );
        doc.set_value(KEY_OCR_BACKEND, json!(OCR_BACKEND_LOCAL_MODEL))
            .expect("合法值");
        assert_eq!(OcrBackend::from_document(&doc), OcrBackend::LocalModel);
        doc.set_value(KEY_OCR_BACKEND, json!(OCR_BACKEND_SYSTEM))
            .expect("合法值");
        assert_eq!(OcrBackend::from_document(&doc), OcrBackend::System);
    }

    /// 选本地模型：直接用本地引擎，无提示。
    #[test]
    fn local_model_selected_directly() {
        let local = fake(OcrBackend::LocalModel, OcrAvailability::Ready);
        let system = fake(OcrBackend::System, OcrAvailability::Ready);
        let sel = select_engine(OcrBackend::LocalModel, local, system);
        assert_eq!(
            (sel.requested, sel.effective),
            (OcrBackend::LocalModel, OcrBackend::LocalModel)
        );
        assert_eq!(sel.notice, None);
    }

    /// 选系统但不可用：回落本地模型，并按不可用原因带对应提示。
    #[test]
    fn unavailable_system_falls_back_with_matching_notice() {
        let cases = [
            (OcrAvailability::NotImplemented, OcrNotice::SystemNotImplemented),
            (OcrAvailability::NoLanguagePack, OcrNotice::SystemNoLanguagePack),
            (OcrAvailability::EngineFailed, OcrNotice::SystemEngineFailed),
        ];
        for (availability, notice) in cases {
            let local = fake(OcrBackend::LocalModel, OcrAvailability::Ready);
            let sel = select_engine(OcrBackend::System, local, fake(OcrBackend::System, availability));
            assert_eq!(sel.requested, OcrBackend::System);
            assert_eq!(sel.effective, OcrBackend::LocalModel);
            assert_eq!(sel.notice, Some(notice));
            let input = OcrInput { width: 1, height: 1, rgba: &[0; 4] };
            assert_eq!(sel.engine.recognize(&input).expect("回落到本地").full_text, "LocalModel");
        }
    }

    /// 系统引擎一旦可用就直接使用，不再提示（P1 接入后的路径）。
    #[test]
    fn ready_system_engine_is_used() {
        let local = fake(OcrBackend::LocalModel, OcrAvailability::Ready);
        let system = fake(OcrBackend::System, OcrAvailability::Ready);
        let sel = select_engine(OcrBackend::System, local, system);
        assert_eq!(sel.effective, OcrBackend::System);
        assert_eq!(sel.notice, None);
    }

    /// 平台探测结果映射成可用性。
    #[test]
    fn status_maps_to_availability() {
        let ready = WinOcrStatus::Ready { languages: vec!["en-US".into()] };
        assert_eq!(availability_from_status(&ready), OcrAvailability::Ready);
        assert_eq!(availability_from_status(&WinOcrStatus::NoLanguagePack), OcrAvailability::NoLanguagePack);
        assert_eq!(availability_from_status(&WinOcrStatus::UnsupportedPlatform), OcrAvailability::NotImplemented);
        assert_eq!(availability_from_status(&WinOcrStatus::EngineFailed("x".into())), OcrAvailability::EngineFailed);
    }

    /// 可用性与提示一一对应，可用时没有提示。
    #[test]
    fn notice_from_availability() {
        assert_eq!(OcrNotice::from_availability(OcrAvailability::Ready), None);
        assert_eq!(
            OcrNotice::from_availability(OcrAvailability::NoLanguagePack),
            Some(OcrNotice::SystemNoLanguagePack)
        );
    }

    /// 平台错误映射：尺寸问题是 InvalidImage，其余是 Failed 且保留原因。
    #[test]
    fn system_errors_are_mapped() {
        assert!(matches!(map_system_error(WinOcrError::InvalidImage("x".into())), OcrError::InvalidImage(_)));
        assert!(matches!(map_system_error(WinOcrError::NoLanguagePack), OcrError::Failed(m) if m.contains("language pack")));
        assert!(matches!(map_system_error(WinOcrError::ImageTooLarge { max: 7 }), OcrError::Failed(m) if m.contains('7')));
    }

    /// 系统行转文本块：置信度为 None、按缩放还原、夹到图内、丢弃空白行。
    #[test]
    fn system_lines_become_boxes() {
        let lines = vec![
            WinOcrLine { text: "ab".into(), rect: [10.4, 20.0, 30.2, 8.0] },
            WinOcrLine { text: "   ".into(), rect: [0.0, 0.0, 5.0, 5.0] },
            WinOcrLine { text: "edge".into(), rect: [90.0, 90.0, 40.0, 40.0] },
        ];
        let boxes = system_lines_to_boxes(&lines, (1.0, 1.0), (100, 100));
        assert_eq!(boxes.len(), 2);
        assert_eq!(boxes[0].confidence, None);
        assert_eq!(boxes[0].rect, PhysicalRect::new(10, 20, 31, 8));
        assert_eq!(boxes[1].rect, PhysicalRect::new(90, 90, 10, 10));
        let scaled = system_lines_to_boxes(&lines[..1], (2.0, 2.0), (200, 200));
        assert_eq!(scaled[0].rect, PhysicalRect::new(20, 40, 62, 16));
    }

    /// 探测缓存：TTL 内复用旧值，过期后重新探测。
    #[test]
    fn probe_cache_respects_ttl() {
        let mut cache = TtlCache { slot: None };
        let t0 = Instant::now();
        let ttl = Duration::from_secs(5);
        let mut calls = 0;
        let mut probe = |value: u8| {
            calls += 1;
            value
        };
        assert_eq!(cache.get_or_probe(t0, ttl, || probe(1)), 1);
        assert_eq!(cache.get_or_probe(t0 + Duration::from_secs(4), ttl, || probe(2)), 1);
        assert_eq!(cache.get_or_probe(t0 + Duration::from_secs(5), ttl, || probe(3)), 3);
        assert_eq!(calls, 2);
    }

    /// 系统引擎对非法图像直接报 InvalidImage，不触碰系统接口（离屏可跑）。
    #[test]
    fn system_engine_rejects_invalid_image() {
        let bad = OcrInput { width: 2, height: 2, rgba: &[0; 3] };
        assert!(matches!(SystemOcr.recognize(&bad), Err(OcrError::InvalidImage(_))));
        let zero = OcrInput { width: 0, height: 5, rgba: &[] };
        assert!(matches!(SystemOcr.recognize(&zero), Err(OcrError::InvalidImage(_))));
        assert_eq!(SystemOcr.backend(), OcrBackend::System);
    }

    /// 真机冒烟：系统 OCR 识别一张渲染出来的英文图（需 Windows 与 OCR 语言包）。
    /// 运行：`cargo test -p snow-shot system_ocr_real -- --ignored --nocapture`。
    #[cfg(windows)]
    #[test]
    #[ignore = "需要真实 Windows 系统 OCR 与语言包"]
    fn system_ocr_real_recognizes_rendered_text() {
        use snow_platform::text_raster::{DEFAULT_FONT_FAMILY, rasterize_text};
        assert_eq!(SystemOcr.availability(), OcrAvailability::Ready);
        let bmp = rasterize_text("Hello Snow Shot", DEFAULT_FONT_FAMILY, 32.0, false).expect("光栅化");
        let (w, h) = (bmp.width + 40, bmp.height + 40);
        let mut rgba = vec![255u8; (w * h * 4) as usize];
        for y in 0..bmp.height {
            for x in 0..bmp.width {
                let v = 255 - bmp.coverage[(y * bmp.width + x) as usize];
                let at = (((20 + y) * w + 20 + x) * 4) as usize;
                rgba[at..at + 4].copy_from_slice(&[v, v, v, 255]);
            }
        }
        let result = SystemOcr.recognize(&OcrInput { width: w, height: h, rgba: &rgba }).expect("识别");
        println!("{result:?}");
        assert!(result.full_text.contains("Snow"), "{}", result.full_text);
        assert!(result.boxes.iter().all(|b| b.confidence.is_none()));
    }

    /// 设置页提示：只有选了系统后端才有提示（内容随本机系统 OCR 状态），值非法时无提示。
    #[test]
    fn notice_for_config_value() {
        assert_eq!(
            OcrNotice::for_config_value(&json!("system")),
            OcrNotice::from_availability(SystemOcr.availability())
        );
        assert_eq!(OcrNotice::for_config_value(&json!("local-model")), None);
        assert_eq!(OcrNotice::for_config_value(&json!("bogus")), None);
        assert_eq!(OcrNotice::for_config_value(&json!(3)), None);
    }

    /// 文案三种语言都有，且互不相同、不含缺失标记。
    #[test]
    fn labels_and_notice_exist_in_all_locales() {
        let mut labels = Vec::new();
        let mut notices = Vec::new();
        for locale in [LOCALE_EN_US, LOCALE_ZH_CN, LOCALE_ZH_TW] {
            for backend in [OcrBackend::System, OcrBackend::LocalModel] {
                let label = backend.label(locale);
                assert!(!label.contains("[!"), "{locale} {backend:?}: {label}");
                labels.push(label);
            }
            for kind in [OcrNotice::SystemNotImplemented, OcrNotice::SystemNoLanguagePack, OcrNotice::SystemEngineFailed] {
                let notice = kind.message(locale);
                assert!(!notice.contains("[!"), "{locale} {kind:?}: {notice}");
                notices.push(notice);
            }
        }
        labels.sort();
        labels.dedup();
        assert_eq!(
            labels.len(),
            6,
            "三种语言 x 两个后端的显示名应各不相同: {labels:?}"
        );
        assert!(notices.iter().all(|n| !n.is_empty()));
        notices.sort();
        notices.dedup();
        assert_eq!(notices.len(), 9, "三种语言 x 三种提示应各不相同: {notices:?}");
        assert_eq!(
            OcrBackend::System.label("fr-FR"),
            OcrBackend::System.label(LOCALE_EN_US)
        );
    }

    /// 本地模型引擎原样转交 `OcrService`：资产缺失时得到原有的 `Unavailable` 错误，非法图像得到 `InvalidImage`。
    #[test]
    fn local_engine_delegates_to_service() {
        let root = std::env::temp_dir().join(format!("snow-ocr-backend-{}", std::process::id()));
        let service = Arc::new(OcrService::with_parts(
            PathBuf::from(&root),
            None,
            Arc::new(crate::ocr_service::ProcessLauncher),
            Duration::from_secs(1),
        ));
        let config = OcrRequestConfig {
            model_kind: "small".into(),
            directml: false,
            resize_policy: 0,
            resident: false,
        };
        let engine = LocalModelOcr::new(service, config);
        assert_eq!(engine.backend(), OcrBackend::LocalModel);
        assert_eq!(engine.availability(), OcrAvailability::Ready);
        let bad = OcrInput {
            width: 2,
            height: 2,
            rgba: &[0; 3],
        };
        assert!(matches!(
            engine.recognize(&bad),
            Err(OcrError::InvalidImage(_))
        ));
        let ok_size = OcrInput {
            width: 1,
            height: 1,
            rgba: &[0; 4],
        };
        assert!(matches!(
            engine.recognize(&ok_size),
            Err(OcrError::Unavailable(OcrUnavailable::NoRuntime))
        ));
    }

    /// 后端选择与提示不含任何密钥字段：该键不属于 `SECRET_KEYS`，Debug 输出只有后端与提示。
    #[test]
    fn backend_key_is_not_secret_and_debug_is_clean() {
        assert!(!crate::settings_model::SECRET_KEYS.contains(&KEY_OCR_BACKEND));
        assert!(crate::settings_model::SECRET_KEYS.contains(&"api_configuration/custom_models"));
        let local = fake(OcrBackend::LocalModel, OcrAvailability::Ready);
        let text = format!(
            "{:?}",
            select_engine(OcrBackend::System, local, fake(OcrBackend::System, OcrAvailability::NotImplemented))
        );
        assert_eq!(
            text,
            "OcrSelection { requested: System, effective: LocalModel, notice: Some(SystemNotImplemented) }"
        );
    }
}
