//! 三栏工作台 PoC：侧栏（会话树）/ 工作区（标签 + 终端）/ Inspector（7 段）。
//!
//! 复刻 Mac 版主工作台的信息架构（见 `02-ui-windows-and-layout.md`）：
//! - 侧栏：source-list 样式会话树，支持搜索、拖拽会话。
//! - 工作区：顶部标签栏（新建 / 切换 / 关闭），下方嵌入终端视图。
//! - Inspector：7 段 segmented control（Files / Tunnels / Browser / Logs / Macros / Command History / AI）。
//! - 文件面板拖放到终端 = 上传（PoC 用 egui DragAndDrop 模拟）。

use std::sync::{Arc, Mutex};

use stacio_term::model::TerminalModel;
use stacio_term::renderer::TerminalRenderer;

use crate::terminal_view::TerminalCallback;

/// Inspector 7 段（与 Mac 一致）。
pub const INSPECTOR_SEGMENTS: [&str; 7] = [
    "Files",
    "Tunnels",
    "Browser",
    "Logs",
    "Macros",
    "Cmd History",
    "AI",
];

/// 会话树节点。
#[derive(Debug, Clone)]
pub struct SessionNode {
    pub id: usize,
    pub name: String,
    pub host: String,
}

#[derive(Debug, Clone)]
pub struct FolderNode {
    pub name: String,
    pub sessions: Vec<SessionNode>,
}

/// 工作区标签。
pub struct Tab {
    pub title: String,
    pub model: Arc<Mutex<TerminalModel>>,
}

/// 拖放载荷：从 Files 面板拖出的"文件"。
#[derive(Debug, Clone)]
pub struct FilePayload {
    pub name: String,
}

/// 工作台状态。
pub struct Workbench {
    pub folders: Vec<FolderNode>,
    pub tabs: Vec<Tab>,
    pub active_tab: usize,
    pub inspector_open: bool,
    pub inspector_seg: usize,
    pub search: String,
    pub uploads: Vec<String>,
    /// Files 面板的本地文件列表（可拖到终端上传）。
    pub local_files: Vec<String>,
    next_session_id: usize,
}

impl Workbench {
    pub fn new(initial_model: Arc<Mutex<TerminalModel>>, initial_title: &str) -> Self {
        let folders = vec![
            FolderNode {
                name: "Production".to_string(),
                sessions: vec![
                    SessionNode { id: 1, name: "web-01".to_string(), host: "10.0.1.10".to_string() },
                    SessionNode { id: 2, name: "web-02".to_string(), host: "10.0.1.11".to_string() },
                    SessionNode { id: 3, name: "db-01".to_string(), host: "10.0.2.10".to_string() },
                ],
            },
            FolderNode {
                name: "Staging".to_string(),
                sessions: vec![
                    SessionNode { id: 4, name: "stg-01".to_string(), host: "10.1.1.10".to_string() },
                    SessionNode { id: 5, name: "stg-02".to_string(), host: "10.1.1.11".to_string() },
                ],
            },
        ];

        Self {
            folders,
            tabs: vec![Tab {
                title: initial_title.to_string(),
                model: initial_model,
            }],
            active_tab: 0,
            inspector_open: true,
            inspector_seg: 0,
            search: String::new(),
            uploads: Vec::new(),
            local_files: ["deploy.sh", "config.yaml", "app.log", "backup.tar.gz", "notes.md"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            next_session_id: 100,
        }
    }

    pub fn open_tab(&mut self, renderer: &Arc<Mutex<TerminalRenderer>>, title: &str) {
        let _ = renderer;
        let model = Arc::new(Mutex::new(TerminalModel::new(stacio_term::model::TerminalSize::new(
            100, 30,
        ))));
        self.tabs.push(Tab {
            title: title.to_string(),
            model,
        });
        self.active_tab = self.tabs.len() - 1;
    }

    pub fn active_model(&self) -> Option<Arc<Mutex<TerminalModel>>> {
        self.tabs.get(self.active_tab).map(|t| t.model.clone())
    }
}

/// 渲染侧栏。返回被双击打开的会话名。
pub fn show_sidebar(ui: &mut egui::Ui, wb: &mut Workbench) -> Option<String> {
    let mut opened = None;

    ui.heading("Sessions");
    ui.add(
        egui::TextEdit::singleline(&mut wb.search)
            .hint_text("Search sessions…")
            .desired_width(f32::INFINITY),
    );
    ui.add_space(6.0);

    egui::ScrollArea::vertical().show(ui, |ui| {
        for folder in &wb.folders {
            egui::CollapsingHeader::new(&folder.name)
                .id_salt(format!("folder-{}", folder.name))
                .default_open(true)
                .show(ui, |ui| {
                    for s in &folder.sessions {
                        if !wb.search.is_empty()
                            && !s.name.contains(&wb.search)
                            && !s.host.contains(&wb.search)
                        {
                            continue;
                        }
                        let label = egui::Label::new(format!("* {}", s.name)).selectable(true);
                        let resp = ui.add(label);
                        // 拖拽会话（PoC：载荷为会话名）。
                        if resp.drag_started() {
                            egui::DragAndDrop::set_payload(ui.ctx(), s.name.clone());
                        }
                        if resp.double_clicked() {
                            opened = Some(s.name.clone());
                        }
                        resp.on_hover_text(format!("{} (double-click to open)", s.host));
                    }
                });
        }
    });

    opened
}

/// 渲染工作区：标签栏 + 终端。
pub fn show_workspace(
    ui: &mut egui::Ui,
    wb: &mut Workbench,
    renderer: &Arc<Mutex<TerminalRenderer>>,
) -> Option<usize> {
    let mut closed = None;

    // 标签栏。
    ui.horizontal(|ui| {
        for (i, tab) in wb.tabs.iter().enumerate() {
            let selected = i == wb.active_tab;
            let resp = ui.selectable_label(selected, format!(" {}", tab.title));
            if resp.clicked() {
                wb.active_tab = i;
            }
            // 关闭按钮。
            let close = ui.small_button("×");
            if close.clicked() {
                closed = Some(i);
            }
            ui.add_space(4.0);
        }
        if ui.small_button("+").clicked() {
            let n = wb.tabs.len() + 1;
            wb.open_tab(renderer, &format!("local-{n}"));
        }
    });
    ui.separator();

    // 终端区域。
    let rect = ui.available_rect_before_wrap();
    if rect.width() < 10.0 || rect.height() < 10.0 {
        return closed;
    }
    let ppi = ui.ctx().pixels_per_point();
    let (cw, ch) = {
        let r = renderer.lock().unwrap();
        let m = r.metrics();
        (m.cell_width, m.cell_height)
    };
    let cols = (rect.width() * ppi / cw) as usize;
    let rows = (rect.height() * ppi / ch) as usize;

    if let Some(model) = wb.active_model() {
        {
            let mut m = model.lock().unwrap();
            let cur = m.size();
            if cur.columns != cols || cur.rows != rows {
                m.resize(stacio_term::model::TerminalSize::new(cols.max(1), rows.max(1)));
            }
        }
        let tw = cols as f32 * cw / ppi;
        let th = rows as f32 * ch / ppi;
        let term_rect = egui::Rect::from_min_size(rect.min, egui::Vec2::new(tw, th));

        // 拖放：文件拖到终端 = 上传。
        let drop_resp = ui.interact(term_rect, egui::Id::new("term-drop"), egui::Sense::hover());
        if let Some(payload) = egui::DragAndDrop::payload::<FilePayload>(ui.ctx()) {
            if drop_resp.hovered() && ui.input(|i| i.pointer.any_released()) {
                let name = payload.name.clone();
                wb.uploads.push(name.clone());
                if let Ok(mut m) = model.lock() {
                    m.process_bytes(format!("\r\n[upload] {name} -> remote\r\n").as_bytes());
                }
                egui::DragAndDrop::clear_payload(ui.ctx());
            }
        }

        let callback = TerminalCallback { model, renderer: renderer.clone() };
        ui.painter().add(egui::Shape::Callback(egui_wgpu::Callback::new_paint_callback(
            term_rect,
            callback,
        )));
    }

    closed
}

/// 渲染 Inspector：7 段 + 内容。
pub fn show_inspector(ui: &mut egui::Ui, wb: &mut Workbench) {
    // segmented control（7 段，窄面板下换行）。
    ui.horizontal_wrapped(|ui| {
        for (i, seg) in INSPECTOR_SEGMENTS.iter().enumerate() {
            let selected = i == wb.inspector_seg;
            if ui.selectable_label(selected, *seg).clicked() {
                wb.inspector_seg = i;
            }
        }
    });
    ui.separator();

    match wb.inspector_seg {
        0 => show_files_pane(ui, wb),
        1 => {
            ui.label("Tunnels: (PoC placeholder)");
        }
        2 => {
            ui.label("Browser: (PoC placeholder)");
        }
        3 => show_logs_pane(ui, wb),
        4 => {
            ui.label("Macros: (PoC placeholder)");
        }
        5 => {
            ui.label("Command History: (PoC placeholder)");
        }
        _ => {
            ui.label("AI Assistant: (PoC placeholder)");
        }
    }
}

/// Files 面板：本地文件列表，可拖到终端上传。
/// "Open…" / "Save…" 调用平台原生文件对话框（PlatformAdapter::FileDialog）。
fn show_files_pane(ui: &mut egui::Ui, wb: &mut Workbench) {
    ui.heading("Local Files");
    ui.add_space(4.0);

    // 原生文件对话框按钮。
    ui.horizontal(|ui| {
        if ui.small_button("Open…").clicked() {
            let adapter = stacio_platform::default_adapter();
            if let Some(path) = adapter.pick_file("Select file to upload") {
                let name = std::path::Path::new(&path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(&path)
                    .to_string();
                wb.local_files.push(name);
            }
        }
        if ui.small_button("Save…").clicked() {
            let adapter = stacio_platform::default_adapter();
            if let Some(path) = adapter.save_file("Save file as", "untitled.txt") {
                let name = std::path::Path::new(&path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(&path)
                    .to_string();
                wb.uploads.push(format!("saved → {name}"));
            }
        }
    });
    ui.add_space(4.0);

    egui::ScrollArea::vertical().show(ui, |ui| {
        for name in &wb.local_files {
            let resp = ui.add(egui::Label::new(format!("- {name}")).selectable(true));
            if resp.drag_started() {
                egui::DragAndDrop::set_payload(
                    ui.ctx(),
                    FilePayload { name: name.clone() },
                );
            }
            resp.on_hover_text("drag onto terminal to upload");
        }
    });
    ui.add_space(6.0);
    ui.separator();
    ui.heading("Recent uploads");
    for u in wb.uploads.iter().rev().take(5) {
        ui.label(format!("> {u}"));
    }
}

fn show_logs_pane(ui: &mut egui::Ui, wb: &mut Workbench) {
    ui.heading("Diagnostics");
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.label("workbench: 3-column layout active");
        ui.label(format!("tabs: {}", wb.tabs.len()));
        ui.label(format!("uploads: {}", wb.uploads.len()));
    });
}
