//! 终端视图的 egui_wgpu 回调：把 `TerminalModel` 内容经 `TerminalRenderer` 画进 egui 帧。

use std::sync::{Arc, Mutex};

use stacio_term::model::TerminalModel;
use stacio_term::renderer::TerminalRenderer;

pub struct TerminalCallback {
    pub model: Arc<Mutex<TerminalModel>>,
    pub renderer: Arc<Mutex<TerminalRenderer>>,
}

impl egui_wgpu::CallbackTrait for TerminalCallback {
    fn prepare(
        &self,
        _device: &wgpu::Device,
        _queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        _callback_resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        // 从模型取渲染快照，重建网格并上传。
        let model = self.model.lock().unwrap();
        let size = model.size();
        let content = model.renderable_content();
        let mut renderer = self.renderer.lock().unwrap();
        renderer.prepare(content, size.columns, size.rows);
        Vec::new()
    }

    fn paint(
        &self,
        info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        _callback_resources: &egui_wgpu::CallbackResources,
    ) {
        let renderer = self.renderer.lock().unwrap();
        let viewport = [
            info.viewport.min.x,
            info.viewport.min.y,
            info.viewport.width(),
            info.viewport.height(),
        ];
        // 终端左上角的绝对屏幕像素坐标。
        let origin_px = [
            info.viewport.min.x * info.pixels_per_point,
            info.viewport.min.y * info.pixels_per_point,
        ];
        let clip = [
            info.clip_rect.min.x,
            info.clip_rect.min.y,
            info.clip_rect.max.x,
            info.clip_rect.max.y,
        ];
        renderer.paint(render_pass, viewport, origin_px, info.pixels_per_point, clip);
    }
}
