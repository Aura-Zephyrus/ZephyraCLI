#[path = "../frontend/app.rs"]
mod frontend;
use frontend::App;
use std::sync::Arc;
#[cfg(debug_assertions)]
use zephyra_core::node::Node;
#[cfg(debug_assertions)]
use zephyra_core::styler::Styler;
use zephyra_style::SheetStyler;
use winit::application::ApplicationHandler;
use winit::event_loop::{ActiveEventLoop, EventLoop};

// 三个后端共用同一个 ApplicationHandler 外壳，按 feature 选宿主类型。
#[cfg(all(windows, feature = "d3d12"))]
use zephyra_winit_d3d12::HostWindow;
#[cfg(all(windows, feature = "d3d11", not(feature = "d3d12")))]
use zephyra_winit::HostWindow;
#[cfg(all(
    feature = "vulkan",
    not(all(windows, feature = "d3d12")),
    not(all(windows, feature = "d3d11", not(feature = "d3d12")))
))]
use zephyra_vulkan::HostWindow;

#[cfg(all(windows, feature = "d3d12"))]
type Host = HostWindow<App>;
#[cfg(all(windows, feature = "d3d11", not(feature = "d3d12")))]
type Host = HostWindow<App>;
#[cfg(all(
    feature = "vulkan",
    not(all(windows, feature = "d3d12")),
    not(all(windows, feature = "d3d11", not(feature = "d3d12")))
))]
type Host = HostWindow<App>;

/// winit 0.30 要求在 `resumed` 中创建窗口，并把窗口事件转发给宿主。
#[cfg(any(
    all(windows, feature = "d3d12"),
    all(windows, feature = "d3d11", not(feature = "d3d12")),
    feature = "vulkan"
))]
struct Bootstrap {
    inner: Option<Host>,
    app: Option<App>,
}

#[cfg(any(
    all(windows, feature = "d3d12"),
    all(windows, feature = "d3d11", not(feature = "d3d12")),
    feature = "vulkan"
))]
impl ApplicationHandler for Bootstrap {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.inner.is_some() {
            return;
        }
        let app = self.app.take().expect("resumed called without an application");
        let mut host = Host::new_lenient(event_loop, app);

        // 样式来源按构建模式切换：--sf 内嵌；普通模式运行时从工程根读文件。
        #[cfg(feature = "sf")]
        let style_src: &str = include_str!("../frontend/app.zstyle");
        #[cfg(not(feature = "sf"))]
        let style_src: String = std::fs::read_to_string("frontend/app.zstyle").unwrap_or_else(|e| {
            panic!("读取 frontend/app.zstyle 失败: {e}（普通模式请从工程根运行，或用 build --sf 内嵌）")
        });
        let parsed = zstyle_compile::parse("app", &style_src);
        assert!(parsed.is_ok(), "app.zstyle 解析失败: {:?}", parsed.errors);
        let sheet = Arc::new(parsed.sheet);

        // Windows 后端在 debug 下支持样式热重载；Vulkan 暂时只用静态 styler。
        #[cfg(all(debug_assertions, not(feature = "vulkan")))]
        {
            let initial = sheet.clone();
            let (styler, handle) = zephyra_fx::dev_reload::reloadable_styler(Arc::new(
                move |root: &mut Node| SheetStyler::new(initial.clone()).apply(root),
            ));
            host.set_styler(Box::new(styler));

            let roots = zephyra_fx::dev_reload::assets_from_env();
            host.set_dev_reloader(zephyra_fx::dev_reload::DevReloader::new(
                &roots,
                move |paths| {
                    if !paths
                        .iter()
                        .any(|p| p.extension().and_then(|s| s.to_str()) == Some("zstyle"))
                    {
                        return;
                    }
                    let file = concat!(env!("CARGO_MANIFEST_DIR"), "/frontend/app.zstyle");
                    let Ok(src) = std::fs::read_to_string(file) else {
                        eprintln!("[zephyra-dev] .zstyle read failed: {file}");
                        return;
                    };
                    let parsed = zstyle_compile::parse("app", &src);
                    if !parsed.is_ok() {
                        eprintln!("[zephyra-dev] .zstyle reload failed ({} error(s)):", parsed.errors.len());
                        for e in &parsed.errors {
                            eprintln!("  {e}");
                        }
                        return;
                    }
                    let next = Arc::new(parsed.sheet);
                    handle.set(Arc::new(move |root: &mut Node| {
                        SheetStyler::new(next.clone()).apply(root)
                    }));
                    eprintln!("[zephyra-dev] .zstyle reloaded: {file}");
                },
            ));
        }
        #[cfg(any(not(debug_assertions), feature = "vulkan"))]
        host.set_styler(Box::new(SheetStyler::new(sheet)));

        self.inner = Some(host);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        id: winit::window::WindowId,
        event: winit::event::WindowEvent,
    ) {
        if let Some(app) = self.inner.as_mut() {
            app.window_event(event_loop, id, event);
        }
    }
}

#[cfg(any(
    all(windows, feature = "d3d12"),
    all(windows, feature = "d3d11", not(feature = "d3d12")),
    feature = "vulkan"
))]
fn main() {
    let event_loop = EventLoop::new().expect("EventLoop::new");
    let mut boot = Bootstrap {
        inner: None,
        app: Some(App::default()),
    };
    if let Err(e) = event_loop.run_app(&mut boot) {
        eprintln!("event loop failed: {e}");
    }
}

#[cfg(not(any(
    all(windows, feature = "d3d12"),
    all(windows, feature = "d3d11", not(feature = "d3d12")),
    feature = "vulkan"
)))]
fn main() {
    eprintln!("需要启用一个渲染后端：--features d3d12 / d3d11 / vulkan");
}
