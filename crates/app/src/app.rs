//! Stacio PoC 应用：winit + egui + wgpu 集成，主界面为终端视图。
//!
//! 运行：
//!   cargo run -p stacio-app            # 空终端（后续接 PTY / StacioCore）
//!   cargo run -p stacio-app -- --stress   # 高强度输出压测

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use stacio_term::model::{TerminalModel, TerminalSize};
use stacio_term::renderer::{FontPair, TerminalRenderer};
use crate::workbench::Workbench;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Icon, Window, WindowId};

/// 加载窗口图标（PNG → RGBA）。
fn load_window_icon() -> Option<Icon> {
    let candidates = [
        std::env::var("STACIO_ICON").ok(),
        Some(format!("{}/../../assets/icons/stacio-32.png", env!("CARGO_MANIFEST_DIR"))),
        Some("assets/icons/stacio-32.png".to_string()),
    ];
    let path = candidates.into_iter().flatten().find(|p| std::path::Path::new(p).exists())?;
    let img = image::open(&path).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    Icon::from_rgba(img.into_raw(), w, h).ok()
}

pub fn run() -> anyhow::Result<()> {
    env_logger::init();

    // 单实例守卫：防止多开。第二实例直接退出。
    // 正式版应把命令行参数（如 stacio:// 链接）转交给首实例，PoC 阶段仅阻止重复启动。
    // acquire() 返回 false 含两种情况：已有实例在跑，或锁原语创建失败（fail-closed）。
    let adapter = stacio_platform::default_adapter();
    if !adapter.acquire() {
        log::warn!("单实例守卫未通过（可能已有实例运行，或系统锁获取失败），本次启动中止。");
        return Ok(());
    }

    let mut args = std::env::args().skip(1);
    let stress = args.any(|a| a == "--stress");
    let screenshot_path = std::env::args()
        .collect::<Vec<_>>()
        .windows(2)
        .find(|w| w[0] == "--screenshot")
        .map(|w| w[1].clone());

    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);

    let mut app = App::new(stress, screenshot_path);
    event_loop.run_app(&mut app)?;
    Ok(())
}

/// wgpu 相关状态，红绘制时整体借用。
struct GpuState {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    surface: wgpu::Surface<'static>,
    surface_config: wgpu::SurfaceConfiguration,
    egui_renderer: egui_wgpu::Renderer,
}

struct App {
    stress: bool,
    /// 捕获截图后退出：`--screenshot <path>`。
    screenshot_path: Option<String>,
    frame_count: u64,
    exit_requested: bool,

    egui_ctx: egui::Context,
    window: Option<Arc<Window>>,
    egui_state: Option<egui_winit::State>,
    gpu: Option<GpuState>,

    /// 三栏工作台（侧栏 / 工作区 / Inspector）。
    workbench: Option<Workbench>,
    terminal_renderer: Option<Arc<Mutex<TerminalRenderer>>>,

    // 压测状态
    stop_signal: Option<Arc<AtomicBool>>,
    stress_thread: Option<std::thread::JoinHandle<()>>,
    bytes_fed: Arc<AtomicU64>,

    // 帧统计
    last_frame: Instant,
    frame_samples: Vec<f64>,
    fps: f32,
    peak_frame_ms: f32,

    // Quick Connect / 终端主题
    quick_connect: String,
    theme_idx: usize,

    // 终端搜索（P4-6）
    search_query: String,
    search_idx: usize,
    search_total: usize,

    // 终端字号（P4-9：Ctrl+滚轮缩放，功能清单 2.14）
    font_size: f32,
}

impl App {
    fn new(stress: bool, screenshot_path: Option<String>) -> Self {
        let egui_ctx = egui::Context::default();
        // 与 Mac 版 Stacio Dark 主题对齐。
        egui_ctx.set_theme(egui::Theme::Dark);
        Self {
            stress,
            screenshot_path,
            frame_count: 0,
            exit_requested: false,
            egui_ctx,
            window: None,
            egui_state: None,
            gpu: None,
            workbench: None,
            terminal_renderer: None,
            stop_signal: None,
            stress_thread: None,
            bytes_fed: Arc::new(AtomicU64::new(0)),
            last_frame: Instant::now(),
            frame_samples: Vec::new(),
            fps: 0.0,
            peak_frame_ms: 0.0,
            quick_connect: String::new(),
            theme_idx: 0,
            search_query: String::new(),
            search_idx: 0,
            search_total: 0,
            font_size: 13.0,
        }
    }

    fn font_bytes() -> anyhow::Result<Vec<u8>> {
        // .app bundle 内字体：Contents/Resources/fonts/。
        let bundle_font = std::env::current_exe()
            .ok()
            .and_then(|exe| {
                exe.parent()
                    .map(|p| p.join("../Resources/fonts/JetBrainsMonoNLNerdFont-Regular.ttf"))
            })
            .map(|p| p.to_string_lossy().into_owned());
        let candidates = [
            std::env::var("STACIO_FONT").ok(),
            bundle_font,
            Some(format!(
                "{}/../../assets/fonts/JetBrainsMonoNLNerdFont-Regular.ttf",
                env!("CARGO_MANIFEST_DIR")
            )),
            Some("assets/fonts/JetBrainsMonoNLNerdFont-Regular.ttf".to_string()),
        ];
        for c in candidates.into_iter().flatten() {
            if let Ok(data) = std::fs::read(&c) {
                return Ok(data);
            }
        }
        anyhow::bail!("未找到终端字体（可设置 STACIO_FONT 指向 .ttf 文件）");
    }

    fn init_resources(&mut self, event_loop: &ActiveEventLoop) -> anyhow::Result<()> {
        let window = Arc::new(event_loop.create_window(
            Window::default_attributes()
                .with_title("Stacio 终端")
                .with_window_icon(load_window_icon())
                .with_inner_size(LogicalSize::new(1100.0, 720.0)),
        )?);

        let native_pixels_per_point = window.scale_factor() as f32;
        let egui_state = egui_winit::State::new(
            self.egui_ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            Some(native_pixels_per_point),
            window.theme(),
            Some(8192),
        );

        // wgpu 实例 + 表面 + 设备。
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            flags: wgpu::InstanceFlags::default(),
            memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
            backend_options: wgpu::BackendOptions::default(),
            display: None,
        });
        let surface = instance.create_surface(window.clone())?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))?;

        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("stacio-device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                experimental_features: wgpu::ExperimentalFeatures::default(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::Off,
            }))?;
        let device = Arc::new(device);
        let queue = Arc::new(queue);

        let caps = surface.get_capabilities(&adapter);
        // egui 偏好非 sRGB 帧缓冲（颜色按 sRGB 值直写）。
        let format = caps
            .formats
            .iter()
            .find(|f| !f.is_srgb())
            .copied()
            .unwrap_or(caps.formats[0]);
        let size = window.inner_size();
        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &surface_config);

        let egui_renderer = egui_wgpu::Renderer::new(&device, format, Default::default());

        // 终端模型 + 渲染器。
        let font_data = Self::font_bytes()?;
        let fonts = FontPair::from_bytes(font_data, None)?;
        let dpi = window.scale_factor() as f32;
        let terminal_renderer = Arc::new(Mutex::new(TerminalRenderer::new(
            device.clone(),
            queue.clone(),
            format,
            fonts,
            13.0,
            dpi,
        )));
        let terminal = Arc::new(Mutex::new(TerminalModel::new(TerminalSize::new(100, 30))));

        // 三栏工作台（初始标签 = 压测目标终端）。
        let workbench = Workbench::new(terminal.clone(), "web-01");

        // 压测模式：后台线程持续注入彩色日志流。
        if self.stress {
            let stop = Arc::new(AtomicBool::new(false));
            let bytes_fed = self.bytes_fed.clone();
            let model = terminal.clone();
            let stop_inner = stop.clone();
            let handle = std::thread::spawn(move || {
                let mut frame: u64 = 0;
                while !stop_inner.load(Ordering::Relaxed) {
                    let burst = stress_burst(frame);
                    bytes_fed.fetch_add(burst.len() as u64, Ordering::Relaxed);
                    if let Ok(mut m) = model.lock() {
                        m.process_bytes(&burst);
                    }
                    frame += 1;
                    std::thread::sleep(Duration::from_millis(4));
                }
            });
            self.stop_signal = Some(stop);
            self.stress_thread = Some(handle);
            log::info!("压测模式已启动：后台线程持续注入输出");
        }

        self.window = Some(window);
        self.egui_state = Some(egui_state);
        self.gpu = Some(GpuState {
            device,
            queue,
            surface,
            surface_config,
            egui_renderer,
        });
        self.workbench = Some(workbench);
        self.terminal_renderer = Some(terminal_renderer);
        Ok(())
    }

    fn build_ui(&mut self, ui: &mut egui::Ui) {
        let Some(renderer) = self.terminal_renderer.clone() else { return };
        let Some(mut wb) = self.workbench.take() else { return };

        let mut search_next = false;
        let mut search_prev = false;

        egui::Panel::top("stats_panel")
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.strong("Stacio 工作台");
                    ui.separator();
                    // 共享核心健康状态（stacio_core 直接依赖）。
                    let h = stacio_core_bridge::CoreHandle::new().health();
                    ui.label(format!(
                        "core {} v{}: {}",
                        h.app,
                        h.version,
                        if h.ok { "ok" } else { "!" }
                    ));
                    ui.label(format!("FPS: {:.0}", self.fps));
                    ui.label(format!("avg: {:.2} ms", self.avg_frame_ms()));
                    ui.label(format!("peak: {:.2} ms", self.peak_frame_ms));
                    if self.stress {
                        let mb = self.bytes_fed.load(Ordering::Relaxed) as f64 / 1e6;
                        ui.label(format!("fed: {mb:.1} MB"));
                    }
                    ui.separator();
                    // Quick Connect：user@host[:port] → SSH 标签。
                    ui.label("快速连接");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.quick_connect)
                            .hint_text("user@host:port")
                            .desired_width(150.0),
                    );
                    if ui.button("连接").clicked() {
                        let handle = stacio_core_bridge::CoreHandle::new();
                        match handle.parse_quick_connect(&self.quick_connect) {
                            Ok(target) if target.protocol.eq_ignore_ascii_case("ssh") => {
                                wb.open_ssh_direct(
                                    &renderer,
                                    &target.host,
                                    target.port,
                                    target.username.as_deref().unwrap_or("root"),
                                );
                                self.quick_connect.clear();
                            }
                            Ok(_) => log::warn!("Quick Connect 暂仅支持 ssh 协议"),
                            Err(e) => log::warn!("Quick Connect 解析失败: {e}"),
                        }
                    }
                    ui.separator();
                    // 终端主题（功能清单 2.6 预设子集）。
                    let themes = stacio_term::renderer::themes::THEMES;
                    egui::ComboBox::from_id_salt("term-theme")
                        .selected_text(themes[self.theme_idx].0)
                        .show_ui(ui, |ui| {
                            for (i, (name, _)) in themes.iter().enumerate() {
                                if ui.selectable_label(self.theme_idx == i, *name).clicked() {
                                    self.theme_idx = i;
                                    if let Ok(mut r) = renderer.lock() {
                                        r.set_palette((themes[i].1)());
                                    }
                                }
                            }
                        });
                    ui.separator();
                    // 终端搜索（功能清单 2.5 子集）。
                    ui.label("⌕");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.search_query)
                            .hint_text("在终端中查找")
                            .desired_width(110.0),
                    );
                    if ui.button("↑").clicked() {
                        search_prev = true;
                    }
                    if ui.button("↓").clicked() {
                        search_next = true;
                    }
                    if self.search_total > 0 {
                        ui.label(format!("{}/{}", self.search_idx + 1, self.search_total));
                    } else {
                        ui.label("0/0");
                    }
                    ui.separator();
                    // 保存终端输出（功能清单 2.24）。
                    if ui.small_button("💾 保存输出").clicked() {
                        if let Some(model) = wb.active_model() {
                            let text = model.lock().unwrap().dump_visible_text();
                            let adapter = stacio_platform::default_adapter();
                            if let Some(path) = adapter.save_file("保存终端输出为", "terminal-output.txt") {
                                if let Err(e) = std::fs::write(&path, text.as_bytes()) {
                                    log::warn!("保存输出失败: {e}");
                                }
                            }
                        }
                    }
                });
            });

        // 侧栏（左）。
        egui::Panel::left("sidebar")
            .exact_size(220.0)
            .show(ui, |ui| {
                let actions = crate::workbench::show_sidebar(ui, &mut wb);
                wb.apply_actions(&renderer, actions);
            });

        // Inspector（右）。
        egui::Panel::right("inspector")
            .exact_size(300.0)
            .show(ui, |ui| {
                crate::workbench::show_inspector(ui, &mut wb);
            });

        // 工作区（中）。
        egui::CentralPanel::default().show(ui, |ui| {
            if let Some(idx) = crate::workbench::show_workspace(ui, &mut wb, &renderer) {
                wb.tabs.remove(idx);
                if wb.active_tab >= wb.tabs.len() && !wb.tabs.is_empty() {
                    wb.active_tab = wb.tabs.len() - 1;
                }
            }
        });

        // 会话 / 文件夹编辑对话框。
        wb.show_edit_dialogs(ui.ctx());

        // 终端搜索：重算匹配 → 推给渲染器高亮；↑/↓ 滚动到当前匹配。
        let matches = if self.search_query.is_empty() {
            Vec::new()
        } else if let Some(model) = wb.active_model() {
            model.lock().unwrap().find_matches(&self.search_query)
        } else {
            Vec::new()
        };
        self.search_total = matches.len();
        if self.search_total > 0 && self.search_idx >= self.search_total {
            self.search_idx = 0;
        }
        if let Ok(mut r) = renderer.lock() {
            r.set_search_matches(matches.clone());
        }
        if (search_next || search_prev) && !matches.is_empty() {
            let n = matches.len();
            self.search_idx = if search_next {
                (self.search_idx + 1) % n
            } else {
                (self.search_idx + n - 1) % n
            };
            if let Some(model) = wb.active_model() {
                let m = matches[self.search_idx];
                model.lock().unwrap().scroll_to_match(&m);
            }
        }

        // Ctrl+滚轮缩放终端字号（功能清单 2.14）。
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        let ctrl = ui.input(|i| i.modifiers.ctrl || i.modifiers.command);
        if ctrl && scroll.abs() > 0.5 {
            self.font_size = (self.font_size + scroll.signum()).clamp(8.0, 32.0);
            if let Ok(mut r) = renderer.lock() {
                let dpi = self.window.as_ref().map(|w| w.scale_factor() as f32).unwrap_or(1.0);
                r.set_font_size(self.font_size, dpi);
            }
        }

        self.workbench = Some(wb);
    }

    fn avg_frame_ms(&self) -> f64 {
        if self.frame_samples.is_empty() {
            return 0.0;
        }
        self.frame_samples.iter().sum::<f64>() / self.frame_samples.len() as f64 * 1000.0
    }

    fn track_frame(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_frame).as_secs_f64();
        self.last_frame = now;
        self.frame_samples.push(dt);
        if self.frame_samples.len() > 120 {
            self.frame_samples.remove(0);
        }
        let avg = self.avg_frame_ms();
        self.fps = if avg > 0.0 { (1000.0 / avg) as f32 } else { 0.0 };
        self.peak_frame_ms = self
            .frame_samples
            .iter()
            .map(|d| (d * 1000.0) as f32)
            .fold(0.0, f32::max);
        // 每 120 帧输出一次性能摘要。
        if self.frame_samples.len() % 120 == 0 {
            let mb = self.bytes_fed.load(Ordering::Relaxed) as f64 / 1e6;
            log::info!(
                "性能: FPS={:.1} 平均={:.2}ms 峰值={:.2}ms 注入={:.1}MB",
                self.fps,
                self.avg_frame_ms(),
                self.peak_frame_ms,
                mb
            );
        }
    }

    fn redraw(&mut self) {
        let window = self.window.clone().unwrap();
        let ctx = self.egui_ctx.clone();

        // 1) 事件输入 → egui UI（终端回调在此注册）。
        let raw_input = self.egui_state.as_mut().unwrap().take_egui_input(&window);
        let full_output = ctx.run_ui(raw_input, |ui| self.build_ui(ui));
        self.egui_state
            .as_mut()
            .unwrap()
            .handle_platform_output(&window, full_output.platform_output);

        let pixels_per_point = full_output.pixels_per_point;
        let clipped = ctx.tessellate(full_output.shapes, pixels_per_point);
        let size_px = window.inner_size();
        let screen_descriptor = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [size_px.width, size_px.height],
            pixels_per_point,
        };

        // 2) GPU 帧：上传 egui 纹理增量（字体图集等），再更新缓冲（回调 prepare 在此运行）。
        let gpu = self.gpu.as_mut().unwrap();
        let textures_delta = &full_output.textures_delta;
        for (id, image_delta) in &textures_delta.set {
            gpu.egui_renderer
                .update_texture(&gpu.device, &gpu.queue, *id, image_delta);
        }
        for id in &textures_delta.free {
            gpu.egui_renderer.free_texture(id);
        }
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("stacio-encoder"),
            });
        let user_cmd_bufs = gpu.egui_renderer.update_buffers(
            &gpu.device,
            &gpu.queue,
            &mut encoder,
            &clipped,
            &screen_descriptor,
        );

        let current = gpu.surface.get_current_texture();
        let surface_texture = match current {
            wgpu::CurrentSurfaceTexture::Success(t) | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            _ => {
                // 超时 / 遮挡等：跳过本帧。
                return;
            }
        };
        let view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        {
            let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("stacio-main-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.16,
                            g: 0.17,
                            b: 0.20,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            gpu.egui_renderer
                .render(&mut pass.forget_lifetime(), &clipped, &screen_descriptor);
        }
        gpu.queue
            .submit(user_cmd_bufs.into_iter().chain(std::iter::once(encoder.finish())));

        // 截图（须在 present 之前，纹理内容仍在）。
        self.frame_count += 1;
        if let Some(path) = self.screenshot_path.clone() {
            if self.frame_count == 60 {
                match capture_frame(gpu, &surface_texture.texture) {
                    Ok(rgba) => match save_png(
                        &path,
                        gpu.surface_config.width,
                        gpu.surface_config.height,
                        &rgba,
                    ) {
                        Ok(()) => {
                            log::info!("截图已保存: {path}");
                            self.exit_requested = true;
                        }
                        Err(e) => log::error!("截图保存失败: {e:#}"),
                    },
                    Err(e) => log::error!("截图失败: {e:#}"),
                }
            }
        }
        surface_texture.present();

        self.track_frame();
        // 持续重绘：光标闪烁 / 压测滚动。
        ctx.request_repaint();
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        if let Err(err) = self.init_resources(event_loop) {
            log::error!("初始化失败: {err:#}");
            event_loop.exit();
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        let window = self.window.clone().unwrap();
        let egui_state = self.egui_state.as_mut().unwrap();
        let egui_response = egui_state.on_window_event(&window, &event);
        if egui_response.repaint {
            window.request_redraw();
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.surface_config.width = size.width.max(1);
                    gpu.surface_config.height = size.height.max(1);
                    gpu.surface.configure(&gpu.device, &gpu.surface_config);
                }
                window.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                if let Some(renderer) = self.terminal_renderer.as_mut() {
                    let mut r = renderer.lock().unwrap();
                    r.set_font_size(13.0, scale_factor as f32);
                }
                window.request_redraw();
            }
            WindowEvent::RedrawRequested => self.redraw(),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.exit_requested {
            event_loop.exit();
            return;
        }
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        if let Some(stop) = &self.stop_signal {
            stop.store(true, Ordering::Relaxed);
        }
        if let Some(handle) = self.stress_thread.take() {
            let _ = handle.join();
        }
    }
}

/// 捕获当前表面帧为 RGBA8 像素（BGRA 表面 → RGBA）。
fn capture_frame(gpu: &GpuState, texture: &wgpu::Texture) -> anyhow::Result<Vec<u8>> {
    let width = gpu.surface_config.width;
    let height = gpu.surface_config.height;
    // wgpu 要求 bytes_per_row 256 对齐。
    let bytes_per_row = ((width * 4) + 255) / 256 * 256;
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("stacio-capture"),
        size: (bytes_per_row as u64) * height as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("stacio-capture-encoder"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([encoder.finish()]);

    let slice = buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |res| {
        let _ = tx.send(res);
    });
    gpu.device
        .poll(wgpu::PollType::Wait { submission_index: None, timeout: None })?;
    rx.recv()??;
    let data = slice.get_mapped_range();
    // BGRA → RGBA（按行跳过 padding）。
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for row in 0..height {
        let row_start = row as usize * bytes_per_row as usize;
        for col in 0..width {
            let i = row_start + col as usize * 4;
            rgba.extend_from_slice(&[data[i + 2], data[i + 1], data[i], data[i + 3]]);
        }
    }
    Ok(rgba)
}

/// 保存 RGBA8 像素为 PNG。
fn save_png(path: &str, width: u32, height: u32, rgba: &[u8]) -> anyhow::Result<()> {
    let file = std::fs::File::create(path)?;
    let w = std::io::BufWriter::new(file);
    let mut encoder = png::Encoder::new(w, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(rgba)?;
    Ok(())
}

/// 生成一段模拟彩色日志流（4ms 一批，混合颜色与换行滚动）。
fn stress_burst(frame: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(4096);
    for i in 0..24u64 {
        let n = frame * 24 + i;
        let line = match i % 6 {
            0 => format!("\x1b[32m[OK]\x1b[0m task {n} completed in {}ms\r\n", n * 3 % 97 + 1),
            1 => format!(
                "\x1b[33m[WARN]\x1b[0m slow response from host-{} ({}ms)\r\n",
                n % 8,
                n * 7 % 500 + 20
            ),
            2 => format!(
                "\x1b[31m[ERROR]\x1b[0m connection reset: 10.0.{}.{}:22\r\n",
                n % 255,
                n % 9
            ),
            3 => format!("\x1b[36m[INFO]\x1b[0m bytes transferred: {} KiB/s\r\n", n * 13 % 4096),
            4 => format!("\x1b[35m[DEBUG]\x1b[0m seq {} delta {}us\r\n", n, n % 1000),
            _ => format!(
                "\x1b[34m[TRACE]\x1b[0m hash={} worker={}\r\n",
                n.wrapping_mul(2654435761) % u64::MAX,
                n % 4
            ),
        };
        out.extend_from_slice(line.as_bytes());
    }
    out
}
