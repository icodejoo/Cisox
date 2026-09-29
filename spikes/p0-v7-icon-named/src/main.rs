//! P0-V7 spike：验证「自有 Ant Design SVG 图标 + gpui-kit `IconNamed` trait + 多色变体」可行。
//! 一次性验证代码，不做抽象。

use gpui_kit::component::{Icon, IconNamed};
use gpui_kit::*;
use std::borrow::Cow;

/// 图标尺寸（px）
const ICON_PX: f32 = 48.0;

/// 次色路径的标记 fill，Ant twotone 约定
const SECONDARY_FILL: &str = "fill=\"#D9D9D9\"";

/// 自有图标枚举，每个变体对应一个真实存在的 Ant SVG 文件
#[derive(Clone, Copy)]
enum AntIcon {
    CameraFilled,
    SettingOutlined,
    AimOutlined,
    BellTwoTone,
    SettingTwoTone,
    ApiTwoTone,
}

/// 只有一个方法：把枚举解析成 AssetSource 能识别的逻辑路径
impl IconNamed for AntIcon {
    fn path(self) -> SharedString {
        match self {
            AntIcon::CameraFilled => "icons/filled/camera.svg",
            AntIcon::SettingOutlined => "icons/outlined/setting.svg",
            AntIcon::AimOutlined => "icons/outlined/aim.svg",
            AntIcon::BellTwoTone => "icons/twotone/bell.svg",
            AntIcon::SettingTwoTone => "icons/twotone/setting.svg",
            AntIcon::ApiTwoTone => "icons/twotone/api.svg",
        }
        .into()
    }
}

/// 承载拆层后的虚拟路径（形如 `icons/twotone/bell.svg#primary`）
struct LayerPath(SharedString);

impl IconNamed for LayerPath {
    fn path(self) -> SharedString {
        self.0
    }
}

/// 多色配色，对应 C++ 侧 IconColors 的概念
#[derive(Clone, Copy)]
struct IconColors {
    primary: Hsla,
    secondary: Option<Hsla>,
}

impl IconColors {
    /// 单色
    fn primary(c: impl Into<Hsla>) -> Self {
        Self {
            primary: c.into(),
            secondary: None,
        }
    }

    /// 双色
    fn two_tone(primary: impl Into<Hsla>, secondary: impl Into<Hsla>) -> Self {
        Self {
            primary: primary.into(),
            secondary: Some(secondary.into()),
        }
    }
}

/// 编译期嵌入的真实 SVG 字节
macro_rules! ant_svg {
    ($rel:literal) => {
        include_bytes!(concat!(
            "E:/workspaces/Cisox/ant_design_qt/packages/ant_design_icons_qt/resources/icons/",
            $rel
        ))
    };
}

/// 自定义资源源：把仓库里的真实 SVG 喂给 GPUI，并在运行时按需拆出单层
struct AntAssets;

impl AssetSource for AntAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        let (base, layer) = match path.split_once('#') {
            Some((b, l)) => (b, Some(l)),
            None => (path, None),
        };

        let raw: &'static [u8] = match base {
            "icons/filled/camera.svg" => ant_svg!("filled/camera.svg"),
            "icons/outlined/setting.svg" => ant_svg!("outlined/setting.svg"),
            "icons/outlined/aim.svg" => ant_svg!("outlined/aim.svg"),
            "icons/twotone/bell.svg" => ant_svg!("twotone/bell.svg"),
            "icons/twotone/setting.svg" => ant_svg!("twotone/setting.svg"),
            "icons/twotone/api.svg" => ant_svg!("twotone/api.svg"),
            _ => return Ok(None),
        };

        match layer {
            None => Ok(Some(Cow::Borrowed(raw))),
            Some(name) => {
                let text = std::str::from_utf8(raw)?;
                let want_primary = name == "primary";
                Ok(Some(Cow::Owned(
                    extract_layer(text, want_primary).into_bytes(),
                )))
            }
        }
    }

    fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
        Ok(vec![])
    }
}

/// 从 twotone SVG 中抽出主色层或次色层，合成一份新的单层 SVG。
/// 判定规则：带 `fill="#D9D9D9"` 的 path 属次色层，其余属主色层。
fn extract_layer(svg: &str, want_primary: bool) -> String {
    let open = svg.find("<svg").expect("缺少 <svg>");
    let header_end = svg[open..].find('>').expect("<svg> 未闭合") + open + 1;
    let close = svg.find("</svg>").unwrap_or(svg.len());

    let mut body = String::new();
    for part in svg[header_end..close].split("<path") {
        if part.trim().is_empty() {
            continue;
        }
        let is_secondary = part.contains(SECONDARY_FILL);
        if is_secondary != want_primary {
            body.push_str("<path");
            body.push_str(part);
        }
    }

    format!("{}{}</svg>", &svg[open..header_end], body)
}

/// 渲染一张图标卡片：单色直接画，双色用绝对定位叠两层
fn icon_card(icon: AntIcon, colors: IconColors, label: &'static str) -> impl IntoElement {
    let base = icon.path();

    let visual = match colors.secondary {
        // 双色：次色层在下、主色层在上
        Some(secondary) => div()
            .relative()
            .w(px(ICON_PX))
            .h(px(ICON_PX))
            .child(
                div().absolute().inset_0().child(
                    Icon::from(LayerPath(format!("{base}#secondary").into()))
                        .text_color(secondary)
                        .w(px(ICON_PX))
                        .h(px(ICON_PX)),
                ),
            )
            .child(
                div().absolute().inset_0().child(
                    Icon::from(LayerPath(format!("{base}#primary").into()))
                        .text_color(colors.primary)
                        .w(px(ICON_PX))
                        .h(px(ICON_PX)),
                ),
            ),
        // 单色
        None => div().w(px(ICON_PX)).h(px(ICON_PX)).child(
            Icon::from(icon)
                .text_color(colors.primary)
                .w(px(ICON_PX))
                .h(px(ICON_PX)),
        ),
    };

    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(8.))
        .child(visual)
        .child(div().text_color(rgb(0x333333)).child(label))
}

/// 验证主视图
struct SpikeView;

impl Render for SpikeView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let blue = rgb(0x1890ff);
        let light_blue = rgb(0x91d5ff);
        let red = rgb(0xf5222d);
        let light_red = rgb(0xffa39e);
        let green = rgb(0x52c41a);

        div()
            .size_full()
            .bg(rgb(0xffffff))
            .text_color(rgb(0x333333))
            .flex()
            .flex_col()
            .gap(px(32.))
            .p(px(32.))
            .child(
                // 第一排：单色图标，三种颜色证明着色生效
                div()
                    .flex()
                    .flex_row()
                    .gap(px(48.))
                    .child(icon_card(
                        AntIcon::CameraFilled,
                        IconColors::primary(blue),
                        "camera filled / blue",
                    ))
                    .child(icon_card(
                        AntIcon::SettingOutlined,
                        IconColors::primary(red),
                        "setting outlined / red",
                    ))
                    .child(icon_card(
                        AntIcon::AimOutlined,
                        IconColors::primary(green),
                        "aim outlined / green",
                    )),
            )
            .child(
                // 第二排：同一 twotone 资源切换两套配色
                div()
                    .flex()
                    .flex_row()
                    .gap(px(48.))
                    .child(icon_card(
                        AntIcon::BellTwoTone,
                        IconColors::two_tone(blue, light_blue),
                        "bell twotone / blue",
                    ))
                    .child(icon_card(
                        AntIcon::BellTwoTone,
                        IconColors::two_tone(red, light_red),
                        "bell twotone / red",
                    ))
                    .child(icon_card(
                        AntIcon::SettingTwoTone,
                        IconColors::two_tone(blue, light_blue),
                        "setting twotone / blue",
                    ))
                    .child(icon_card(
                        AntIcon::ApiTwoTone,
                        IconColors::two_tone(red, light_red),
                        "api twotone / red",
                    )),
            )
    }
}

fn main() {
    gpui_kit::application()
        .with_assets(AntAssets)
        .run(|cx: &mut App| {
            gpui_kit::init(cx);
            let bounds = Bounds::centered(None, size(px(920.), px(420.)), cx);
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                cx,
                |_, cx| cx.new(|_| SpikeView),
            )
            .expect("打开窗口失败");
        });
}
