mod download;
mod downloads;
mod music;
mod session;
mod ui;

use gpui::{Application, Bounds, WindowBounds, WindowOptions, prelude::*, px, size};

fn main() {
    Application::new().run(|cx| {
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(1060.0), px(700.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(750.0), px(510.0))),
                ..Default::default()
            },
            |window, cx| {
                window.set_window_title("音乐下载");
                cx.new(ui::Desktop::new)
            },
        )
        .expect("无法打开音乐下载窗口");
        cx.activate(true);
    });
}
