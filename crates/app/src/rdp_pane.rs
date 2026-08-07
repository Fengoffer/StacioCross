//! RDP 会话面板（功能清单 6.7）。
//!
//! 复用 core 的 `RdpSession`（IronRDP 内嵌客户端）：
//! - `RdpSessionDelegate::on_frame` 在后台线程交付 BGRA8888 脏矩形
//! - UI 每帧把最新帧上传为 egui 纹理显示
//! - 鼠标 / 键盘转发（send_pointer / send_key / send_unicode）

use std::sync::{Arc, Mutex};

use stacio_core_bridge::{RdpSecurityMode, RdpSession, RdpSessionDelegate};

/// 跨线程共享的 RDP 帧与状态（delegate 后台线程写，UI 读）。
#[derive(Default)]
pub struct SharedRdp {
    pub frame: Mutex<Option<(u32, u32, Vec<u8>)>>, // (w, h, bgra)
    pub status: Mutex<String>,
    pub disconnected_reason: Mutex<Option<String>>,
}

impl SharedRdp {
    pub fn new() -> Self {
        Self {
            frame: Mutex::new(None),
            status: Mutex::new("未连接".to_owned()),
            disconnected_reason: Mutex::new(None),
        }
    }
}

/// RDP delegate：接收帧 / 状态回调，并请求 UI 重绘。
struct RdpDelegate {
    shared: Arc<SharedRdp>,
    ctx: egui::Context,
}

impl RdpSessionDelegate for RdpDelegate {
    fn on_frame(
        &self,
        desktop_width: u32,
        desktop_height: u32,
        _x: u32,
        _y: u32,
        _width: u32,
        _height: u32,
        bgra: Vec<u8>,
    ) {
        // PoC：保存整帧（脏矩形合并为整帧上传）。
        *self.shared.frame.lock().unwrap() = Some((desktop_width, desktop_height, bgra));
        self.ctx.request_repaint();
    }
    fn on_pointer_visibility(&self, _visible: bool) {}
    fn on_pointer_position(&self, _x: u32, _y: u32) {}
    fn on_pointer_bitmap(&self, _w: u32, _h: u32, _hx: u32, _hy: u32, _rgba: Vec<u8>) {}
    fn on_clipboard(&self, _text: String) {}
    fn on_network_status(&self, _rtt_ms: u32, _mode: String) {}
    fn on_disconnected(&self, reason: String) {
        *self.shared.status.lock().unwrap() = format!("已断开：{reason}");
        *self.shared.disconnected_reason.lock().unwrap() = Some(reason);
        self.ctx.request_repaint();
    }
}

/// RDP 面板状态。
pub struct RdpPaneState {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub session: Option<Arc<RdpSession>>,
    pub shared: Arc<SharedRdp>,
    /// 已上传的 egui 纹理。
    pub texture: Option<egui::TextureHandle>,
}

impl RdpPaneState {
    pub fn new() -> Self {
        Self {
            host: String::new(),
            port: 3389,
            username: String::new(),
            password: String::new(),
            session: None,
            shared: Arc::new(SharedRdp::new()),
            texture: None,
        }
    }

    /// 发起连接。
    pub fn connect(&mut self, ctx: &egui::Context) {
        let session = RdpSession::new();
        let host = self.host.clone();
        let port = self.port;
        let username = self.username.clone();
        let password = self.password.clone();
        let shared = self.shared.clone();
        *shared.status.lock().unwrap() = "连接中…".to_owned();
        let delegate: Box<dyn RdpSessionDelegate> =
            Box::new(RdpDelegate { shared: shared.clone(), ctx: ctx.clone() });
        session.clone().connect(
            host,
            port,
            username,
            password,
            None,
            1024,
            768,
            85,
            RdpSecurityMode::Nla,
            true, // PoC：忽略证书校验
            None,
            None,
            delegate,
        );
        self.session = Some(session);
    }

    /// 关闭连接。
    pub fn close(&self) {
        if let Some(s) = &self.session {
            s.close();
        }
    }
}

impl Default for RdpPaneState {
    fn default() -> Self {
        Self::new()
    }
}
