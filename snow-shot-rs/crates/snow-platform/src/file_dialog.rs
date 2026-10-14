//! 原生“另存为”与“打开文件”对话框（Windows `IFileSaveDialog`），不依赖任何 GUI 框架。

use std::path::PathBuf;

/// 文件类型过滤项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileFilter {
    /// 显示名（如 `PNG image (*.png)`）。
    pub label: String,
    /// 匹配模式，多个用分号分隔（如 `*.jpg;*.jpeg`）。
    pub pattern: String,
}

/// 另存为对话框请求。
#[derive(Debug, Clone, Default)]
pub struct SaveDialogRequest {
    /// 对话框标题。
    pub title: String,
    /// 初始目录；`None` 或不存在时由系统决定。
    pub initial_dir: Option<PathBuf>,
    /// 预填文件名（含扩展名）。
    pub file_name: String,
    /// 过滤项列表；为空时不限制类型。
    pub filters: Vec<FileFilter>,
    /// 初始选中的过滤项下标（从 0 开始）。
    pub filter_index: usize,
    /// 所属窗口句柄（`HWND` 的整数值）；`None` 时无所有者。
    pub owner: Option<isize>,
}

/// 用户在对话框里的选择。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveDialogChoice {
    /// 选中的完整路径。
    pub path: PathBuf,
    /// 最终选中的过滤项下标（从 0 开始）。
    pub filter_index: usize,
}

/// “打开文件”对话框请求。
#[derive(Debug, Clone, Default)]
pub struct OpenDialogRequest {
    /// 对话框标题。
    pub title: String,
    /// 初始目录；`None` 或不存在时由系统决定。
    pub initial_dir: Option<PathBuf>,
    /// 过滤项列表；为空时不限制类型。
    pub filters: Vec<FileFilter>,
    /// 所属窗口句柄（`HWND` 的整数值）；`None` 时无所有者。
    pub owner: Option<isize>,
}

/// 把字符串转成以 0 结尾的 UTF-16 缓冲。
///
/// # 参数
/// - `text`：原文。
///
/// # 返回
/// 以 0 结尾的 UTF-16 序列。
///
/// ```
/// assert_eq!(snow_platform::file_dialog::to_wide("a"), vec![97u16, 0]);
/// ```
pub fn to_wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 把初始过滤下标收敛到合法范围（过滤项为空时返回 0）。
///
/// # 参数
/// - `index`：期望下标。
/// - `len`：过滤项个数。
///
/// ```
/// assert_eq!(snow_platform::file_dialog::clamp_filter_index(9, 3), 2);
/// assert_eq!(snow_platform::file_dialog::clamp_filter_index(1, 0), 0);
/// ```
pub fn clamp_filter_index(index: usize, len: usize) -> usize {
    index.min(len.saturating_sub(1))
}

/// 弹出“另存为”对话框并阻塞到用户选择或取消。
///
/// # 参数
/// - `request`：对话框参数。
///
/// # 返回
/// 用户确认返回 `Some(选择)`，取消返回 `None`；系统调用失败返回错误说明。
/// 覆盖已有文件时由系统弹出确认。非 Windows 平台恒返回错误。
///
/// ```ignore
/// let choice = show_save_dialog(&SaveDialogRequest::default())?;
/// ```
pub fn show_save_dialog(request: &SaveDialogRequest) -> Result<Option<SaveDialogChoice>, String> {
    imp::show(request)
}

/// 弹出“打开文件”对话框并阻塞到用户选择或取消。
///
/// # 参数
/// - `request`：对话框参数。
///
/// # 返回
/// 用户确认返回 `Some(路径)`，取消返回 `None`；系统调用失败返回错误说明。非 Windows 平台恒返回错误。
///
/// ```ignore
/// let path = show_open_dialog(&OpenDialogRequest::default())?;
/// ```
pub fn show_open_dialog(request: &OpenDialogRequest) -> Result<Option<PathBuf>, String> {
    imp::show_open(request)
}

#[cfg(windows)]
mod imp {
    use super::{
        FileFilter, OpenDialogRequest, SaveDialogChoice, SaveDialogRequest, clamp_filter_index,
        to_wide,
    };
    use std::path::{Path, PathBuf};
    use windows::Win32::Foundation::{HWND, RPC_E_CHANGED_MODE};
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        CoTaskMemFree,
    };
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    use windows::Win32::UI::Shell::{
        FILEOPENDIALOGOPTIONS, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FOS_OVERWRITEPROMPT,
        FOS_PATHMUSTEXIST, FileOpenDialog, FileSaveDialog, IFileDialog, IFileOpenDialog,
        IFileSaveDialog, IShellItem, SHCreateItemFromParsingName, SIGDN_FILESYSPATH,
    };
    use windows::core::PCWSTR;

    /// 用户取消对话框时系统返回的 HRESULT（`HRESULT_FROM_WIN32(ERROR_CANCELLED)`）。
    const HRESULT_CANCELLED: i32 = 0x8007_04C7_u32 as i32;

    /// 初始化当前线程的 COM（单线程套间）；线程已是别的套间模式时忽略（不反初始化）。
    fn init_com() -> Result<(), String> {
        // SAFETY: 标准的线程级 COM 初始化。
        let init = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if init.is_err() && init != RPC_E_CHANGED_MODE {
            return Err(format!("初始化 COM 失败: {init:?}"));
        }
        Ok(())
    }

    /// 过滤项的宽字符缓冲与对应的 `COMDLG_FILTERSPEC` 数组；须活到对话框关闭之后。
    struct FilterSpecs {
        /// 持有 `specs` 里指针指向的缓冲（`Vec` 堆内存不随结构体移动）。
        _text: Vec<(Vec<u16>, Vec<u16>)>,
        /// 传给系统的描述数组。
        specs: Vec<COMDLG_FILTERSPEC>,
    }

    impl FilterSpecs {
        /// 由过滤项列表构造。
        fn new(filters: &[FileFilter]) -> Self {
            let text: Vec<(Vec<u16>, Vec<u16>)> = filters
                .iter()
                .map(|f| (to_wide(&f.label), to_wide(&f.pattern)))
                .collect();
            let specs = text
                .iter()
                .map(|(label, pattern)| COMDLG_FILTERSPEC {
                    pszName: PCWSTR(label.as_ptr()),
                    pszSpec: PCWSTR(pattern.as_ptr()),
                })
                .collect();
            Self { _text: text, specs }
        }
    }

    /// 标题的宽字符缓冲；空标题返回 `None`（保持系统默认）。
    fn title_buffer(title: &str) -> Option<Vec<u16>> {
        (!title.is_empty()).then(|| to_wide(title))
    }

    /// 配置打开 / 保存对话框共有的部分：选项、标题、过滤项与初始目录。
    ///
    /// # Safety
    /// `title` 与 `filters` 里的缓冲必须在对话框关闭前保持有效。
    unsafe fn configure(
        dialog: &IFileDialog,
        extra_options: FILEOPENDIALOGOPTIONS,
        title: Option<&[u16]>,
        filters: &FilterSpecs,
        filter_index: usize,
        initial_dir: Option<&Path>,
    ) -> Result<(), String> {
        // SAFETY: dialog 有效；所有指针指向调用方仍存活的缓冲。
        unsafe {
            let options = dialog.GetOptions().map_err(|e| e.to_string())?;
            dialog
                .SetOptions(options | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST | extra_options)
                .map_err(|e| e.to_string())?;
            if let Some(title) = title {
                dialog
                    .SetTitle(PCWSTR(title.as_ptr()))
                    .map_err(|e| e.to_string())?;
            }
            if !filters.specs.is_empty() {
                dialog
                    .SetFileTypes(&filters.specs)
                    .map_err(|e| e.to_string())?;
                let index = clamp_filter_index(filter_index, filters.specs.len()) as u32 + 1;
                dialog.SetFileTypeIndex(index).map_err(|e| e.to_string())?;
            }
            if let Some(dir) = initial_dir.filter(|d| d.is_dir()) {
                let wide = to_wide(&dir.to_string_lossy());
                // 目录不可解析时放弃预设，让系统用默认位置
                if let Ok(item) =
                    SHCreateItemFromParsingName::<_, _, IShellItem>(PCWSTR(wide.as_ptr()), None)
                {
                    let _ = dialog.SetFolder(&item);
                }
            }
        }
        Ok(())
    }

    /// 弹出对话框并取回所选的文件系统路径；用户取消返回 `Ok(None)`。
    ///
    /// # Safety
    /// 传给 `dialog` 的所有缓冲必须在本函数返回前保持有效。
    unsafe fn show_and_pick(
        dialog: &IFileDialog,
        owner: Option<isize>,
        failure_label: &str,
    ) -> Result<Option<PathBuf>, String> {
        let owner = HWND(owner.unwrap_or(0) as *mut core::ffi::c_void);
        // SAFETY: dialog 有效；返回的显示名由我们用 CoTaskMemFree 释放。
        unsafe {
            if let Err(e) = dialog.Show(Some(owner)) {
                return if e.code().0 == HRESULT_CANCELLED {
                    Ok(None)
                } else {
                    Err(format!("{failure_label}: {e}"))
                };
            }
            let item = dialog.GetResult().map_err(|e| e.to_string())?;
            let raw = item
                .GetDisplayName(SIGDN_FILESYSPATH)
                .map_err(|e| e.to_string())?;
            let path = raw.to_string().map_err(|e| e.to_string());
            CoTaskMemFree(Some(raw.0 as *const core::ffi::c_void));
            Ok(Some(PathBuf::from(path?)))
        }
    }

    /// 显示“打开文件”对话框（Windows 实现）。
    pub fn show_open(request: &OpenDialogRequest) -> Result<Option<PathBuf>, String> {
        init_com()?;
        // SAFETY: COM 已初始化，创建进程内的系统对话框对象。
        let dialog: IFileOpenDialog =
            unsafe { CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER) }
                .map_err(|e| format!("创建打开对话框失败: {e}"))?;
        // 以下缓冲都要活到 Show 返回之后
        let title = title_buffer(&request.title);
        let filters = FilterSpecs::new(&request.filters);
        // SAFETY: 缓冲在本函数内存活到对话框关闭。
        unsafe {
            configure(
                &dialog,
                FOS_FILEMUSTEXIST,
                title.as_deref(),
                &filters,
                0,
                request.initial_dir.as_deref(),
            )?;
            show_and_pick(&dialog, request.owner, "打开对话框失败")
        }
    }

    /// 显示对话框（Windows 实现）。
    pub fn show(request: &SaveDialogRequest) -> Result<Option<SaveDialogChoice>, String> {
        init_com()?;
        // SAFETY: COM 已初始化，创建进程内的系统对话框对象。
        let dialog: IFileSaveDialog =
            unsafe { CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER) }
                .map_err(|e| format!("创建保存对话框失败: {e}"))?;
        // 以下缓冲都要活到 Show 返回之后
        let title = title_buffer(&request.title);
        let name = title_buffer(&request.file_name);
        let filters = FilterSpecs::new(&request.filters);
        // SAFETY: 缓冲在本函数内存活到对话框关闭。
        unsafe {
            configure(
                &dialog,
                FOS_OVERWRITEPROMPT,
                title.as_deref(),
                &filters,
                request.filter_index,
                request.initial_dir.as_deref(),
            )?;
            if let Some(name) = &name {
                dialog
                    .SetFileName(PCWSTR(name.as_ptr()))
                    .map_err(|e| e.to_string())?;
            }
            let Some(path) = show_and_pick(&dialog, request.owner, "保存对话框失败")? else {
                return Ok(None);
            };
            let filter_index = dialog
                .GetFileTypeIndex()
                .map(|i| (i as usize).saturating_sub(1))
                .unwrap_or(0);
            Ok(Some(SaveDialogChoice { path, filter_index }))
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::{OpenDialogRequest, SaveDialogChoice, SaveDialogRequest};
    use std::path::PathBuf;

    /// 非 Windows 平台暂无原生打开对话框。
    pub fn show_open(_request: &OpenDialogRequest) -> Result<Option<PathBuf>, String> {
        Err("当前平台不支持原生打开对话框".to_string())
    }

    /// 非 Windows 平台暂无原生对话框。
    pub fn show(_request: &SaveDialogRequest) -> Result<Option<SaveDialogChoice>, String> {
        Err("当前平台不支持原生保存对话框".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 宽字符串以 0 结尾并保留非 ASCII 字符。
    #[test]
    fn wide_is_nul_terminated() {
        assert_eq!(to_wide(""), vec![0]);
        assert_eq!(to_wide("截图"), vec![0x622A, 0x56FE, 0]);
    }

    /// 过滤下标越界时收敛到最后一项，空列表为 0。
    #[test]
    fn filter_index_is_clamped() {
        assert_eq!(clamp_filter_index(0, 3), 0);
        assert_eq!(clamp_filter_index(5, 3), 2);
        assert_eq!(clamp_filter_index(5, 0), 0);
    }
}
