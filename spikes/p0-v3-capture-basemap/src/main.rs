use gpui::*;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 显示采集底图的根视图
struct Overlay {
    image_source: ImageSource,
    /// 采集发起时刻，用于统计首帧延迟
    start_time: Instant,
    /// 是否已经完成首帧渲染打点
    first_frame_reported: bool,
}

impl Render for Overlay {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        if !self.first_frame_reported {
            self.first_frame_reported = true;
            let t = self.start_time;
            // 在本帧真正绘制完成后打点，逼近“画面实际出现在窗口上”的时刻
            window.on_next_frame(move |_, _| {
                println!("[LATENCY] first-frame-presented: {:?}", t.elapsed());
            });
            println!("[LATENCY] first-render-called: {:?}", self.start_time.elapsed());
        }
        div()
            .w_full()
            .h_full()
            .child(
                img(self.image_source.clone())
                    .w_full()
                    .h_full()
                    .object_fit(ObjectFit::Contain)
            )
    }
}

/// P0-V3 验证入口：采一帧主屏 → 转 BGRA 预乘 → RenderImage → GPUI 窗口显示，并报告首帧延迟
fn main() {
    println!("Starting capture...");
    let start_time = Instant::now();

    // 采集一帧主显示器画面
    let target = snow_capture::CaptureTarget::PrimaryMonitor;
    let frame = snow_capture::capture_once(&target).expect("Failed to capture screen");
    
    let w = frame.width();
    let h = frame.height();
    let format = frame.pixel_format();
    let mut data = frame.as_bytes().to_vec();
    
    println!("Captured frame {}x{} format {:?}", w, h, format);
    
    // RenderImage 要求 BGRA + 预乘 alpha
    for pixel in data.chunks_exact_mut(4) {
        if format == snow_capture::CapturePixelFormat::Rgba8 {
            // 交换 R/B 通道
            let r = pixel[0];
            pixel[0] = pixel[2];
            pixel[2] = r;
        }
        // alpha 预乘
        let a = pixel[3] as u32;
        if a != 255 {
            pixel[0] = ((pixel[0] as u32 * a) / 255) as u8;
            pixel[1] = ((pixel[1] as u32 * a) / 255) as u8;
            pixel[2] = ((pixel[2] as u32 * a) / 255) as u8;
        }
    }
    
    let rgba_image = image::RgbaImage::from_vec(w, h, data).expect("Failed to create RgbaImage");
    let image_frame = image::Frame::new(rgba_image);
    let render_image = gpui::RenderImage::new(vec![image_frame]);
    let image_source = ImageSource::Render(Arc::new(render_image));
    
    let capture_latency = start_time.elapsed();
    println!("Capture and conversion took: {:?}", capture_latency);
    
    gpui_platform::application().run(move |cx: &mut App| {
        let displays = cx.displays();
        // 用一个 960x600 的普通窗口显示采集底图，方便肉眼/截图核对内容是否正确
        let bounds = Bounds {
            origin: point(px(120.0), px(120.0)),
            size: size(px(960.0), px(600.0)),
        };

        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_background: WindowBackgroundAppearance::Opaque,
            display_id: Some(displays[0].id()),
            ..Default::default()
        };
        
        cx.open_window(options, |_, cx| {
            cx.new(|_| Overlay {
                image_source: image_source.clone(),
                start_time,
                first_frame_reported: false,
            })
        })
        .unwrap();
        
        let display_latency = start_time.elapsed();
        println!("Time to open window: {:?}", display_latency);
        
        // Auto-close after 3 seconds
        cx.spawn(async move |app: &mut AsyncApp| {
            app.background_executor().timer(Duration::from_secs(6)).await;
            let _ = app.update(|cx| cx.quit());
        })
        .detach();
    });
}
