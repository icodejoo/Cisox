//! 图标系统：Ant Design SVG 资源、`IconRef`/`IconColors` 模型与 resvg 光栅化。
//!
//! 本 crate 不依赖 gpui，输出为纯 RGBA 缓冲，由 snow-ui-shell 负责喂给 GPUI。
//!
//! ```
//! use snow_ui_icons::{IconRef, IconRenderer, IconRequest, IconTheme};
//!
//! let renderer = IconRenderer::new();
//! let icon = IconRef::new(IconTheme::Outlined, "setting");
//! let bmp = renderer.render(&icon, &IconRequest::square(24, 2.0)).unwrap();
//! assert_eq!((bmp.width, bmp.height), (48, 48));
//! ```

mod assets;
mod model;
mod render;

pub use assets::{IconLayers, icon_count, icon_names, mask_layers, template_svg};
pub use model::{IconColors, IconFit, IconPalette, IconRef, IconTheme, ResolvedColors, Rgba};
pub use render::{CacheStats, IconBitmap, IconRenderer, IconRequest};
