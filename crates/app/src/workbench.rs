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
    /// stacio_core 的会话 id（String）。
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub protocol: String,
}

#[derive(Debug, Clone)]
pub struct FolderNode {
    /// stacio_core 的文件夹 id（String）。
    pub id: String,
    pub name: String,
    /// 子文件夹（多级树）。
    pub folders: Vec<FolderNode>,
    pub sessions: Vec<SessionNode>,
}

/// 工作区标签。
pub struct Tab {
    pub title: String,
    pub model: Arc<Mutex<TerminalModel>>,
    pub kind: TabKind,
}

/// 标签内容类型：本地终端 vs SSH 会话（P4-2）。
pub enum TabKind {
    Local,
    Ssh(Arc<Mutex<crate::ssh_tab::SshTabState>>),
}

/// 拖放载荷：从 Files 面板拖出的"文件"。
#[derive(Debug, Clone)]
pub struct FilePayload {
    pub name: String,
}

/// 会话编辑草稿（新建 = id None；编辑 = id Some）。
#[derive(Debug, Clone)]
pub struct SessionEditDraft {
    pub id: Option<String>,
    pub folder_id: Option<String>,
    pub name: String,
    pub protocol: String,
    pub host: String,
    pub port: u32,
    pub username: String,
}

/// 文件夹编辑草稿（新建 = id None；重命名 = id Some）。
#[derive(Debug, Clone)]
pub struct FolderEditDraft {
    pub id: Option<String>,
    pub parent_id: Option<String>,
    pub name: String,
}

/// 侧栏右键动作（show_sidebar 收集，由 app 统一执行）。
#[derive(Debug, Clone)]
pub enum SidebarAction {
    /// 打开会话（SSH → SSH 标签）。
    OpenSession(SessionNode),
    /// 编辑会话。
    EditSession(SessionNode),
    /// 删除会话。
    DeleteSession(String),
    /// 在文件夹下新建会话（folder_id None = 顶层）。
    NewSession(Option<String>),
    /// 新建文件夹（parent_id）。
    NewFolder(Option<String>),
    /// 重命名文件夹。
    RenameFolder(String),
    /// 删除文件夹。
    DeleteFolder(String),
}

/// 工作台状态。
pub struct Workbench {
    pub folders: Vec<FolderNode>,
    pub tabs: Vec<Tab>,
    pub active_tab: usize,
    pub inspector_seg: usize,
    pub search: String,
    pub uploads: Vec<String>,
    /// Files 面板的本地文件列表（可拖到终端上传）。
    pub local_files: Vec<String>,
    /// 会话编辑对话框（None = 关闭）。
    pub session_edit: Option<SessionEditDraft>,
    /// 文件夹编辑对话框（None = 关闭）。
    pub folder_edit: Option<FolderEditDraft>,
}

impl Workbench {
    pub fn new(initial_model: Arc<Mutex<TerminalModel>>, initial_title: &str) -> Self {
        // 会话树来自 stacio_core 真实库（正式实施阶段）；空库时侧栏显示空态提示。
        let folders = load_session_tree();

        Self {
            folders,
            tabs: vec![Tab {
                title: initial_title.to_string(),
                model: initial_model,
                kind: TabKind::Local,
            }],
            active_tab: 0,
            inspector_seg: 0,
            search: String::new(),
            uploads: Vec::new(),
            local_files: ["deploy.sh", "config.yaml", "app.log", "backup.tar.gz", "notes.md"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            session_edit: None,
            folder_edit: None,
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
            kind: TabKind::Local,
        });
        self.active_tab = self.tabs.len() - 1;
    }

    /// 打开 SSH 会话标签（Auth 阶段起步）。
    pub fn open_ssh_tab(&mut self, renderer: &Arc<Mutex<TerminalRenderer>>, session: &SessionNode) {
        let _ = renderer;
        let model = Arc::new(Mutex::new(TerminalModel::new(stacio_term::model::TerminalSize::new(
            100, 30,
        ))));
        let state = crate::ssh_tab::SshTabState::new(
            &session.host,
            session.port,
            session.username.as_deref().unwrap_or("root"),
        );
        self.tabs.push(Tab {
            title: format!("{}@{}", state.username, state.host),
            model,
            kind: TabKind::Ssh(Arc::new(Mutex::new(state))),
        });
        self.active_tab = self.tabs.len() - 1;
    }

    pub fn active_model(&self) -> Option<Arc<Mutex<TerminalModel>>> {
        self.tabs.get(self.active_tab).map(|t| t.model.clone())
    }

    /// 处理侧栏动作：打开 / 编辑 / 删除 / 新建…
    pub fn apply_actions(
        &mut self,
        renderer: &Arc<Mutex<TerminalRenderer>>,
        actions: Vec<SidebarAction>,
    ) {
        let handle = stacio_core_bridge::CoreHandle::new();
        for action in actions {
            match action {
                SidebarAction::OpenSession(node) => {
                    if node.protocol.eq_ignore_ascii_case("ssh") {
                        self.open_ssh_tab(renderer, &node);
                    } else {
                        self.open_tab(renderer, &node.name);
                    }
                }
                SidebarAction::EditSession(node) => {
                    self.session_edit = Some(SessionEditDraft {
                        id: Some(node.id.clone()),
                        folder_id: None,
                        name: node.name.clone(),
                        protocol: node.protocol.clone(),
                        host: node.host.clone(),
                        port: node.port as u32,
                        username: node.username.clone().unwrap_or_default(),
                    });
                }
                SidebarAction::DeleteSession(id) => {
                    let _ = handle.delete_session(&id);
                    self.reload_sessions();
                }
                SidebarAction::NewSession(folder_id) => {
                    self.session_edit = Some(SessionEditDraft {
                        id: None,
                        folder_id,
                        name: String::new(),
                        protocol: "ssh".to_string(),
                        host: String::new(),
                        port: 22,
                        username: String::new(),
                    });
                }
                SidebarAction::NewFolder(parent_id) => {
                    self.folder_edit = Some(FolderEditDraft {
                        id: None,
                        parent_id,
                        name: String::new(),
                    });
                }
                SidebarAction::RenameFolder(id) => {
                    let name = self.folder_name(&id).unwrap_or_default();
                    self.folder_edit = Some(FolderEditDraft {
                        id: Some(id),
                        parent_id: None,
                        name,
                    });
                }
                SidebarAction::DeleteFolder(id) => {
                    let _ = handle.delete_folder(&id);
                    self.reload_sessions();
                }
            }
        }
    }

    /// 重新加载会话树（增删改之后）。
    pub fn reload_sessions(&mut self) {
        self.folders = load_session_tree();
    }

    /// 按 id 找文件夹名（树查找）。
    fn folder_name(&self, id: &str) -> Option<String> {
        fn walk(folders: &[FolderNode], id: &str) -> Option<String> {
            for f in folders {
                if f.id == id {
                    return Some(f.name.clone());
                }
                if let Some(n) = walk(&f.folders, id) {
                    return Some(n);
                }
            }
            None
        }
        walk(&self.folders, id)
    }

    /// 保存会话编辑（新建或更新）。
    pub fn save_session_edit(&mut self) {
        let Some(draft) = self.session_edit.take() else { return };
        let handle = stacio_core_bridge::CoreHandle::new();
        let username = if draft.username.is_empty() {
            None
        } else {
            Some(draft.username.clone())
        };
        match &draft.id {
            Some(id) => {
                let update = stacio_core_bridge::SessionUpdate {
                    name: Some(draft.name.clone()),
                    protocol: Some(draft.protocol.clone()),
                    folder_id: draft.folder_id.clone(),
                    host: Some(draft.host.clone()),
                    port: Some(draft.port),
                    username,
                    private_key_path: None,
                    credential_id: None,
                    tags: None,
                    config_json: None,
                };
                if let Err(e) = handle.update_session(id, update) {
                    log::warn!("更新会话失败: {e}");
                }
            }
            None => {
                let d = stacio_core_bridge::SessionDraft {
                    folder_id: draft.folder_id.clone(),
                    name: draft.name.clone(),
                    protocol: draft.protocol.clone(),
                    host: draft.host.clone(),
                    port: draft.port,
                    username,
                    private_key_path: None,
                    credential_id: None,
                    tags: vec![],
                    config_json: None,
                };
                if let Err(e) = handle.create_session(d) {
                    log::warn!("创建会话失败: {e}");
                }
            }
        }
        self.reload_sessions();
    }

    /// 保存文件夹编辑（新建或重命名）。
    pub fn save_folder_edit(&mut self) {
        let Some(draft) = self.folder_edit.take() else { return };
        let handle = stacio_core_bridge::CoreHandle::new();
        match &draft.id {
            Some(id) => {
                if let Err(e) = handle.rename_folder(id, &draft.name) {
                    log::warn!("重命名文件夹失败: {e}");
                }
            }
            None => {
                if let Err(e) = handle.create_folder(draft.parent_id.as_deref(), &draft.name) {
                    log::warn!("创建文件夹失败: {e}");
                }
            }
        }
        self.reload_sessions();
    }

    /// 渲染会话 / 文件夹编辑对话框。
    pub fn show_edit_dialogs(&mut self, ctx: &egui::Context) {
        let mut save_session = false;
        let mut cancel_session = false;
        if let Some(draft) = &mut self.session_edit {
            let title = if draft.id.is_some() { "编辑会话" } else { "新建会话" };
            egui::Window::new(title).collapsible(false).resizable(false).show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("名称");
                    ui.text_edit_singleline(&mut draft.name);
                });
                ui.horizontal(|ui| {
                    ui.label("协议");
                    egui::ComboBox::from_id_salt("sess-protocol")
                        .selected_text(&draft.protocol)
                        .show_ui(ui, |ui| {
                            for p in ["ssh", "sftp", "scp", "telnet", "serial"] {
                                ui.selectable_value(&mut draft.protocol, p.to_string(), p);
                            }
                        });
                });
                ui.horizontal(|ui| {
                    ui.label("主机");
                    ui.text_edit_singleline(&mut draft.host);
                });
                ui.horizontal(|ui| {
                    ui.label("端口");
                    ui.add(egui::DragValue::new(&mut draft.port).range(1..=65535));
                });
                ui.horizontal(|ui| {
                    ui.label("用户名");
                    ui.text_edit_singleline(&mut draft.username);
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("保存").clicked() {
                        save_session = true;
                    }
                    if ui.button("取消").clicked() {
                        cancel_session = true;
                    }
                });
            });
        }
        if save_session {
            self.save_session_edit();
        } else if cancel_session {
            self.session_edit = None;
        }

        let mut save_folder = false;
        let mut cancel_folder = false;
        if let Some(draft) = &mut self.folder_edit {
            let title = if draft.id.is_some() { "重命名文件夹" } else { "新建文件夹" };
            egui::Window::new(title).collapsible(false).resizable(false).show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("名称");
                    ui.text_edit_singleline(&mut draft.name);
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("保存").clicked() {
                        save_folder = true;
                    }
                    if ui.button("取消").clicked() {
                        cancel_folder = true;
                    }
                });
            });
        }
        if save_folder {
            self.save_folder_edit();
        } else if cancel_folder {
            self.folder_edit = None;
        }
    }
}

/// 从 stacio_core 加载真实会话树；失败/空库时返回空列表。
fn load_session_tree() -> Vec<FolderNode> {
    let handle = stacio_core_bridge::CoreHandle::new();
    match handle.session_sidebar_snapshot() {
        Ok(snap) => build_folder_tree(&snap),
        Err(e) => {
            log::warn!("加载会话树失败: {e}");
            Vec::new()
        }
    }
}

/// 把 core 快照（扁平 folders + 带 folder_id 的 sessions）构造成嵌套树。
/// 无文件夹的会话归入 "Ungrouped"；子文件夹挂在父文件夹下。
fn build_folder_tree(snap: &stacio_core_bridge::SessionSidebarSnapshot) -> Vec<FolderNode> {
    use std::collections::HashMap;

    let mut by_id: HashMap<&str, FolderNode> = HashMap::new();
    for f in &snap.folders {
        by_id.insert(
            f.id.as_str(),
            FolderNode {
                id: f.id.clone(),
                name: f.name.clone(),
                folders: Vec::new(),
                sessions: Vec::new(),
            },
        );
    }
    for s in &snap.sessions {
        let node = SessionNode {
            id: s.id.clone(),
            name: s.name.clone(),
            host: s.host.clone(),
            port: s.port as u16,
            username: s.username.clone(),
            protocol: s.protocol.clone(),
        };
        match s.folder_id.as_deref().and_then(|id| by_id.get_mut(id)) {
            Some(folder) => folder.sessions.push(node),
            None => {
                let entry = by_id.entry("__ungrouped__").or_insert_with(|| FolderNode {
                    id: "__ungrouped__".to_string(),
                    name: "Ungrouped".to_string(),
                    folders: Vec::new(),
                    sessions: Vec::new(),
                });
                entry.sessions.push(node);
            }
        }
    }

    let mut roots: Vec<FolderNode> = Vec::new();
    for f in &snap.folders {
        let node = by_id.remove(f.id.as_str()).expect("folder exists");
        match f.parent_id.as_deref() {
            Some(parent) => {
                if let Some(p) = by_id.get_mut(parent) {
                    p.folders.push(node);
                } else {
                    roots.push(node); // 父不存在则提升为顶层
                }
            }
            None => roots.push(node),
        }
    }
    if let Some(u) = by_id.remove("__ungrouped__") {
        roots.push(u);
    }
    roots
}

/// 递归渲染文件夹（含子文件夹与会话）。动作收集到 `actions`。
fn show_folder(
    ui: &mut egui::Ui,
    wb: &Workbench,
    folder: &FolderNode,
    path: &str,
    opened: &mut Option<SessionNode>,
    actions: &mut Vec<SidebarAction>,
) {
    let salt = format!("folder-{path}");
    let collapsing = egui::CollapsingHeader::new(&folder.name)
        .id_salt(salt)
        .default_open(true)
        .show(ui, |ui| {
            for child in &folder.folders {
                show_folder(ui, wb, child, &format!("{path}/{}", child.name), opened, actions);
            }
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
                    *opened = Some(s.clone());
                }
                let _ = resp
                    .clone()
                    .on_hover_text(format!("{}:{} (double-click to open)", s.host, s.port));
                // 会话右键菜单：编辑 / 删除。
                resp.context_menu(|ui| {
                    if ui.button("编辑…").clicked() {
                        actions.push(SidebarAction::EditSession(s.clone()));
                        ui.close();
                    }
                    if ui.button("删除").clicked() {
                        actions.push(SidebarAction::DeleteSession(s.id.clone()));
                        ui.close();
                    }
                });
            }
        });
    // 文件夹 header 右键菜单。
    collapsing.header_response.context_menu(|ui| {
        if ui.button("新建会话…").clicked() {
            actions.push(SidebarAction::NewSession(Some(folder.id.clone())));
            ui.close();
        }
        if ui.button("新建子文件夹…").clicked() {
            actions.push(SidebarAction::NewFolder(Some(folder.id.clone())));
            ui.close();
        }
        if ui.button("重命名…").clicked() {
            actions.push(SidebarAction::RenameFolder(folder.id.clone()));
            ui.close();
        }
        if ui.button("删除文件夹").clicked() {
            actions.push(SidebarAction::DeleteFolder(folder.id.clone()));
            ui.close();
        }
    });
}

/// 渲染侧栏。返回收集到的动作（打开 / 编辑 / 删除 / 新建…）。
pub fn show_sidebar(ui: &mut egui::Ui, wb: &mut Workbench) -> Vec<SidebarAction> {
    let mut opened = None;
    let mut actions = Vec::new();

    ui.horizontal(|ui| {
        ui.heading("Sessions");
        if ui.small_button("＋会话").clicked() {
            actions.push(SidebarAction::NewSession(None));
        }
        if ui.small_button("＋文件夹").clicked() {
            actions.push(SidebarAction::NewFolder(None));
        }
    });
    ui.add(
        egui::TextEdit::singleline(&mut wb.search)
            .hint_text("Search sessions…")
            .desired_width(f32::INFINITY),
    );
    ui.add_space(6.0);

    if wb.folders.is_empty() {
        // 空库提示（正式实施阶段：会话来自 stacio_core 数据库）。
        ui.add_space(12.0);
        ui.label("会话库为空");
        ui.small(format!("数据库: {}", stacio_core_bridge::CoreHandle::new().db_path()));
        ui.small("点「＋会话」新建，或右键会话编辑 / 删除");
    } else {
        egui::ScrollArea::vertical().show(ui, |ui| {
            for folder in &wb.folders {
                show_folder(ui, wb, folder, &folder.name, &mut opened, &mut actions);
            }
        });
    }

    if let Some(s) = opened {
        actions.push(SidebarAction::OpenSession(s));
    }
    actions
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
            // 关闭按钮（SSH 标签同时关闭 live shell 运行时）。
            let close = ui.small_button("×");
            if close.clicked() {
                if let TabKind::Ssh(s) = &wb.tabs[i].kind {
                    crate::ssh_tab::close_runtime(s);
                }
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

        // SSH 标签：按阶段渲染（认证表单 / 指纹确认 / 运行终端）。
        if let TabKind::Ssh(state) = &wb.tabs[wb.active_tab].kind {
            let state = state.clone();
            let running = matches!(
                state.lock().unwrap().phase,
                crate::ssh_tab::SshPhase::Running { .. }
            );
            if running {
                let rid = match &state.lock().unwrap().phase {
                    crate::ssh_tab::SshPhase::Running { runtime_id } => runtime_id.clone(),
                    _ => unreachable!(),
                };
                crate::ssh_tab::report_resize(&state, cols as u32, rows as u32);
                let callback = TerminalCallback { model: model.clone(), renderer: renderer.clone() };
                ui.painter().add(egui::Shape::Callback(egui_wgpu::Callback::new_paint_callback(
                    term_rect,
                    callback,
                )));
                capture_terminal_input(ui, &rid, term_rect);
            } else {
                let mut st = state.lock().unwrap();
                render_ssh_phase_ui(ui, &mut st, &state, &model);
            }
            return closed;
        }

        // 本地终端：拖放上传 + 渲染。
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

/// SSH 标签的非运行阶段 UI：认证表单 / 指纹确认 / 失败重试。
fn render_ssh_phase_ui(
    ui: &mut egui::Ui,
    st: &mut crate::ssh_tab::SshTabState,
    state: &Arc<Mutex<crate::ssh_tab::SshTabState>>,
    model: &Arc<Mutex<TerminalModel>>,
) {
    use crate::ssh_tab::SshPhase;
    match &st.phase {
        SshPhase::Auth => {
            ui.add_space(12.0);
            ui.heading("SSH 连接");
            ui.label(format!("{}:{}", st.host, st.port));
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label("用户名");
                ui.text_edit_singleline(&mut st.username);
            });
            ui.horizontal(|ui| {
                ui.label("密码");
                ui.add(egui::TextEdit::singleline(&mut st.password).password(true));
            });
            ui.checkbox(&mut st.use_agent, "使用 SSH Agent");
            ui.add_space(6.0);
            if ui.button("连接").clicked() {
                let s = state.clone();
                let m = model.clone();
                crate::ssh_tab::begin_connect(&s, m);
            }
        }
        SshPhase::Busy(message) => {
            ui.add_space(16.0);
            ui.label(format!("⏳ {message}"));
        }
        SshPhase::ConfirmHostKey {
            fingerprint,
            previous,
            ..
        } => {
            ui.add_space(12.0);
            ui.heading("主机密钥确认");
            if previous.is_some() {
                ui.colored_label(egui::Color32::from_rgb(220, 90, 90), "⚠ 主机密钥已变更！");
                ui.label("如果这是你预期的变更，请选择「信任并连接」。");
            } else {
                ui.label("首次连接该主机，请核对指纹：");
            }
            ui.add_space(6.0);
            ui.label(format!("SHA256: {fingerprint}"));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("信任并连接").clicked() {
                    let s = state.clone();
                    let m = model.clone();
                    crate::ssh_tab::confirm_host_key(&s, m);
                }
                if ui.button("取消").clicked() {
                    st.phase = SshPhase::Auth;
                }
            });
        }
        SshPhase::Failed { message } => {
            ui.add_space(12.0);
            ui.colored_label(egui::Color32::from_rgb(220, 90, 90), format!("连接失败: {message}"));
            ui.add_space(6.0);
            if ui.button("返回").clicked() {
                st.phase = SshPhase::Auth;
            }
        }
        SshPhase::Closed => {
            ui.add_space(16.0);
            ui.label("会话已关闭");
        }
        SshPhase::Running { .. } => {}
    }
}

/// 捕获键盘输入并写入 core（SSH live shell 输入方向）。
fn capture_terminal_input(
    ui: &mut egui::Ui,
    runtime_id: &str,
    term_rect: egui::Rect,
) {
    let resp = ui.interact(term_rect, egui::Id::new("term-ssh-input"), egui::Sense::click());
    if resp.clicked() {
        resp.request_focus();
    }
    if !resp.has_focus() {
        return;
    }
    let events: Vec<egui::Event> = ui.input(|i| i.events.clone());
    let modifiers = ui.input(|i| i.modifiers);
    let mut bytes: Vec<u8> = Vec::new();
    for ev in events {
        match ev {
            egui::Event::Text(t) => {
                // Ctrl/⌘ 组合键由 Key 分支处理（避免控制字符重复发送）。
                if !modifiers.ctrl && !modifiers.command {
                    bytes.extend_from_slice(t.as_bytes());
                }
            }
            egui::Event::Key {
                key,
                pressed,
                modifiers,
                ..
            } => {
                if pressed {
                    if let Some(b) = crate::ssh_tab::terminal_key_bytes(key, modifiers) {
                        bytes.extend_from_slice(&b);
                    }
                }
            }
            _ => {}
        }
    }
    if !bytes.is_empty() {
        let _ = stacio_core_bridge::CoreHandle::new().write_input(runtime_id, bytes);
    }
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
