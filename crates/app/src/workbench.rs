//! 三栏工作台 PoC：侧栏（会话树）/ 工作区（标签 + 终端）/ Inspector（7 段）。
//!
//! 复刻 Mac 版主工作台的信息架构（见 `02-ui-windows-and-layout.md`）：
//! - 侧栏：source-list 样式会话树，支持搜索、拖拽会话。
//! - 工作区：顶部标签栏（新建 / 切换 / 关闭），下方嵌入终端视图。
//! - Inspector：7 段 segmented control（Files / Tunnels / Browser / Logs / Macros / Command History / AI）。
//! - 文件面板拖放到终端 = 上传（PoC 用 egui DragAndDrop 模拟）。

use std::sync::{Arc, Mutex};

use stacio_core_bridge::ScpDirection;
use stacio_term::model::TerminalModel;
use stacio_term::renderer::TerminalRenderer;

use crate::terminal_view::TerminalCallback;

/// Inspector 7 段（与 Mac 一致）。
pub const INSPECTOR_SEGMENTS: [&str; 7] = [
    "文件",
    "隧道",
    "浏览器",
    "日志",
    "宏",
    "命令历史",
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

/// 终端窗格（分屏的最小单元）。
pub struct Pane {
    pub model: Arc<Mutex<TerminalModel>>,
    /// SSH 标签：与主窗格共享状态（同一连接，多视图）。
    pub ssh: Option<Arc<Mutex<crate::ssh_tab::SshTabState>>>,
}

impl Pane {
    fn local(model: Arc<Mutex<TerminalModel>>) -> Self {
        Self { model, ssh: None }
    }
}

/// 分屏布局模式（功能清单 2.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitMode {
    /// 单窗格。
    Single,
    /// 垂直分屏（左右）。
    Vertical,
    /// 水平分屏（上下）。
    Horizontal,
}

/// 工作区标签。
pub struct Tab {
    pub title: String,
    pub model: Arc<Mutex<TerminalModel>>,
    pub kind: TabKind,
    /// 分屏窗格（index 0 为主窗格，使用 self.model）。
    pub panes: Vec<Pane>,
    pub split: SplitMode,
}

impl Tab {
    fn local(title: String, model: Arc<Mutex<TerminalModel>>) -> Self {
        Self {
            title,
            model: model.clone(),
            kind: TabKind::Local,
            panes: vec![Pane::local(model)],
            split: SplitMode::Single,
        }
    }

    fn ssh(
        title: String,
        model: Arc<Mutex<TerminalModel>>,
        state: Arc<Mutex<crate::ssh_tab::SshTabState>>,
    ) -> Self {
        Self {
            title,
            model: model.clone(),
            kind: TabKind::Ssh(state.clone()),
            panes: vec![Pane { model, ssh: Some(state) }],
            split: SplitMode::Single,
        }
    }

    /// 追加一个分屏窗格（共享 SSH 状态或新建本地终端）。
    pub fn add_pane(&mut self) {
        let model = Arc::new(Mutex::new(TerminalModel::new(stacio_term::model::TerminalSize::new(80, 24))));
        let ssh = match &self.kind {
            TabKind::Ssh(s) => Some(s.clone()),
            _ => None,
        };
        self.panes.push(Pane { model, ssh });
        // 自动切换布局：第 2 个窗格用垂直，第 3+ 用水平网格简化。
        self.split = match self.panes.len() {
            2 => SplitMode::Vertical,
            _ => SplitMode::Horizontal,
        };
    }
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
    /// Files 面板：本地真实浏览器。
    pub local_browser: crate::files_pane::LocalBrowser,
    /// Files 面板：SFTP 远程浏览。
    pub remote_fs: Arc<Mutex<crate::files_pane::RemoteFsState>>,
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
            tabs: vec![Tab::local(initial_title.to_string(), initial_model)],
            active_tab: 0,
            inspector_seg: 0,
            search: String::new(),
            uploads: Vec::new(),
            local_browser: crate::files_pane::LocalBrowser::new(),
            remote_fs: Arc::new(Mutex::new(crate::files_pane::RemoteFsState::new())),
            session_edit: None,
            folder_edit: None,
        }
    }

    pub fn open_tab(&mut self, renderer: &Arc<Mutex<TerminalRenderer>>, title: &str) {
        let _ = renderer;
        let model = Arc::new(Mutex::new(TerminalModel::new(stacio_term::model::TerminalSize::new(
            100, 30,
        ))));
        self.tabs.push(Tab::local(title.to_string(), model));
        self.active_tab = self.tabs.len() - 1;
    }

    /// 打开会话标签（按协议：ssh / telnet / serial；其他协议暂以本地终端打开）。
    pub fn open_ssh_tab(&mut self, renderer: &Arc<Mutex<TerminalRenderer>>, session: &SessionNode) {
        let _ = renderer;
        let model = Arc::new(Mutex::new(TerminalModel::new(stacio_term::model::TerminalSize::new(
            100, 30,
        ))));
        let state = match session.protocol.to_ascii_lowercase().as_str() {
            "telnet" => crate::ssh_tab::SshTabState::new_telnet(&session.host, session.port),
            "serial" => crate::ssh_tab::SshTabState::new_serial(&session.host, 115_200),
            _ => crate::ssh_tab::SshTabState::new(
                &session.host,
                session.port,
                session.username.as_deref().unwrap_or("root"),
            ),
        };
        self.tabs.push(Tab::ssh(
            session.name.clone(),
            model,
            Arc::new(Mutex::new(state)),
        ));
        self.active_tab = self.tabs.len() - 1;
    }

    /// 快速连接：直接以 host/port/username 开 SSH 标签（对应 `parse_quick_connect`）。
    pub fn open_ssh_direct(
        &mut self,
        renderer: &Arc<Mutex<TerminalRenderer>>,
        host: &str,
        port: u16,
        username: &str,
    ) {
        let _ = renderer;
        let model = Arc::new(Mutex::new(TerminalModel::new(stacio_term::model::TerminalSize::new(
            100, 30,
        ))));
        let state = crate::ssh_tab::SshTabState::new(host, port, username);
        self.tabs.push(Tab::ssh(
            format!("{username}@{host}"),
            model,
            Arc::new(Mutex::new(state)),
        ));
        self.active_tab = self.tabs.len() - 1;
    }

    pub fn active_model(&self) -> Option<Arc<Mutex<TerminalModel>>> {
        // 分屏后取主窗格（pane 0）的 model，与 self.model 同一实例。
        self.tabs.get(self.active_tab).map(|t| t.panes.first().map(|p| p.model.clone()).unwrap_or_else(|| t.model.clone()))
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
                    name: "未分组".to_string(),
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
                    .on_hover_text(format!("{}:{} (双击打开)", s.host, s.port));
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
        ui.heading("会话");
        if ui.small_button("＋会话").clicked() {
            actions.push(SidebarAction::NewSession(None));
        }
        if ui.small_button("＋文件夹").clicked() {
            actions.push(SidebarAction::NewFolder(None));
        }
    });
    ui.add(
        egui::TextEdit::singleline(&mut wb.search)
            .hint_text("搜索会话…")
            .desired_width(f32::INFINITY),
    );
    ui.add_space(6.0);

    if wb.folders.is_empty() {
        // 空库提示（正式实施阶段：会话来自 stacio_core 数据库）。
        ui.add_space(12.0);
        ui.label("会话库为空");
        ui.small(format!("数据库：{}", stacio_core_bridge::CoreHandle::new().db_path()));
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

/// 渲染工作区：标签栏 + 终端（含分屏，功能清单 2.3）。
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
        ui.separator();
        // 分屏按钮（作用于当前标签）。
        if ui.small_button("⊞ 分屏").clicked() {
            if let Some(tab) = wb.tabs.get_mut(wb.active_tab) {
                if tab.panes.len() < 4 {
                    tab.add_pane();
                }
            }
        }
        if ui.small_button("⊟ 取消分屏").clicked() {
            if let Some(tab) = wb.tabs.get_mut(wb.active_tab) {
                if tab.panes.len() > 1 {
                    tab.panes.pop();
                    tab.split = if tab.panes.len() <= 1 {
                        SplitMode::Single
                    } else {
                        tab.split
                    };
                }
            }
        }
    });
    ui.separator();

    // 终端区域。
    let rect = ui.available_rect_before_wrap();
    if rect.width() < 10.0 || rect.height() < 10.0 {
        return closed;
    }

    let tab = match wb.tabs.get_mut(wb.active_tab) {
        Some(t) => t,
        None => return closed,
    };
    let n_panes = tab.panes.len();
    let split = tab.split;
    // 克隆窗格的 Arc 引用，避免在渲染时再借用 wb.tabs / wb.uploads。
    let pane_models: Vec<(Arc<Mutex<TerminalModel>>, Option<Arc<Mutex<crate::ssh_tab::SshTabState>>>)> = tab
        .panes
        .iter()
        .map(|p| (p.model.clone(), p.ssh.clone()))
        .collect();

    // 按分屏模式切分 rect。
    let pane_rects: Vec<egui::Rect> = match split {
        SplitMode::Single => vec![rect],
        SplitMode::Vertical => {
            // 左右：每列均分；n>2 时简化为第一列独占 + 其余堆右（PoC）。
            let mut v = Vec::new();
            let mut x = rect.left();
            for i in 0..n_panes {
                let w = if i == n_panes - 1 {
                    rect.right() - x
                } else {
                    rect.width() / n_panes as f32
                };
                v.push(egui::Rect::from_min_size(
                    egui::pos2(x, rect.top()),
                    egui::Vec2::new(w, rect.height()),
                ));
                x += w;
            }
            v
        }
        SplitMode::Horizontal => {
            // 上下：每行均分。
            let mut v = Vec::new();
            let mut y = rect.top();
            for i in 0..n_panes {
                let h = if i == n_panes - 1 {
                    rect.bottom() - y
                } else {
                    rect.height() / n_panes as f32
                };
                v.push(egui::Rect::from_min_size(
                    egui::pos2(rect.left(), y),
                    egui::Vec2::new(rect.width(), h),
                ));
                y += h;
            }
            v
        }
    };

    // 逐窗格渲染（pane_models 已 clone Arc，无 wb 借用冲突）。
    for (i, (model, ssh)) in pane_models.into_iter().enumerate() {
        let pane = Pane { model, ssh };
        render_pane(ui, &pane, &pane_rects[i], renderer, &mut wb.uploads);
    }

    closed
}

/// 渲染单个终端窗格（resize + SSH 阶段分发 / 本地拖放 + 渲染 + 输入捕获）。
fn render_pane(
    ui: &mut egui::Ui,
    pane: &Pane,
    rect: &egui::Rect,
    renderer: &Arc<Mutex<TerminalRenderer>>,
    uploads: &mut Vec<String>,
) {
    if rect.width() < 10.0 || rect.height() < 10.0 {
        return;
    }
    let ppi = ui.ctx().pixels_per_point();
    let (cw, ch) = {
        let r = renderer.lock().unwrap();
        let m = r.metrics();
        (m.cell_width, m.cell_height)
    };
    let cols = (rect.width() * ppi / cw) as usize;
    let rows = (rect.height() * ppi / ch) as usize;
    let model = pane.model.clone();
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

    // SSH 窗格：按阶段渲染。
    if let Some(state) = &pane.ssh {
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
            capture_terminal_input(ui, &state, &rid, term_rect);
            // 多行粘贴确认对话框（功能清单 2.18）。
            let paste = state.lock().unwrap().pending_paste.clone();
            if let Some(clip) = paste {
                let mut ok = false;
                let mut cancel = false;
                egui::Window::new("多行粘贴确认")
                    .collapsible(false)
                    .resizable(false)
                    .show(ui.ctx(), |ui| {
                        ui.label(format!("将粘贴 {} 行内容到终端：", clip.lines().count()));
                        ui.add_space(4.0);
                        let preview: String = clip.lines().take(5).collect::<Vec<_>>().join("\n");
                        ui.label(&preview);
                        if clip.lines().count() > 5 {
                            ui.weak(format!("…（共 {} 行）", clip.lines().count()));
                        }
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if ui.button("粘贴").clicked() {
                                ok = true;
                            }
                            if ui.button("取消").clicked() {
                                cancel = true;
                            }
                        });
                    });
                if ok {
                    let _ = stacio_core_bridge::CoreHandle::new().write_input(&rid, clip.into_bytes());
                    state.lock().unwrap().pending_paste = None;
                }
                if cancel {
                    state.lock().unwrap().pending_paste = None;
                }
            }
        } else {
            let mut st = state.lock().unwrap();
            render_ssh_phase_ui(ui, &mut st, &state, &model);
        }
        return;
    }

    // 本地终端：拖放上传 + 渲染。
    let drop_resp = ui.interact(term_rect, egui::Id::new("term-drop"), egui::Sense::hover());
    if let Some(payload) = egui::DragAndDrop::payload::<FilePayload>(ui.ctx()) {
        if drop_resp.hovered() && ui.input(|i| i.pointer.any_released()) {
            let name = payload.name.clone();
            uploads.push(name.clone());
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

/// SSH 标签的非运行阶段 UI：认证表单 / 指纹确认 / 失败重试。
fn render_ssh_phase_ui(
    ui: &mut egui::Ui,
    st: &mut crate::ssh_tab::SshTabState,
    state: &Arc<Mutex<crate::ssh_tab::SshTabState>>,
    model: &Arc<Mutex<TerminalModel>>,
) {
    use crate::ssh_tab::{ShellKind, SshPhase};
    match &st.phase {
        SshPhase::Auth => {
            ui.add_space(12.0);
            let heading = match st.kind {
                ShellKind::Ssh => "SSH 连接",
                ShellKind::Telnet => "Telnet 连接",
                ShellKind::Serial => "串口连接",
            };
            ui.heading(heading);
            ui.label(format!("{}:{}", st.host, st.port));
            ui.add_space(6.0);
            match st.kind {
                ShellKind::Ssh => {
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
                }
                ShellKind::Telnet => {
                    ui.label("无需认证，直接连接。");
                    ui.add_space(6.0);
                }
                ShellKind::Serial => {
                    ui.horizontal(|ui| {
                        ui.label("设备");
                        ui.text_edit_singleline(&mut st.host);
                    });
                    ui.horizontal(|ui| {
                        ui.label("波特率");
                        ui.add(egui::DragValue::new(&mut st.baud_rate).range(300..=921600));
                    });
                    ui.add_space(6.0);
                }
            }
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
/// 多行粘贴（功能清单 2.18）：Cmd/Ctrl+V 时若剪贴板含多行，存 pending_paste 待确认。
fn capture_terminal_input(
    ui: &mut egui::Ui,
    state: &Arc<Mutex<crate::ssh_tab::SshTabState>>,
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
    let mut pending_multi: Option<String> = None;
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
            // 粘贴事件：egui 已从系统剪贴板取到文本。
            egui::Event::Paste(clip) => {
                if clip.lines().count() > 1 {
                    // 多行：暂存待确认。
                    pending_multi = Some(clip);
                } else {
                    bytes.extend_from_slice(clip.as_bytes());
                }
            }
            _ => {}
        }
    }

    // 多行粘贴待确认（对话框在 render_pane Running 分支弹）。
    if let Some(clip) = pending_multi {
        state.lock().unwrap().pending_paste = Some(clip);
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
            ui.label("隧道：（占位）");
        }
        2 => {
            ui.label("浏览器：（占位）");
        }
        3 => show_logs_pane(ui, wb),
        4 => {
            ui.label("宏：（占位）");
        }
        5 => {
            ui.label("命令历史：（占位）");
        }
        _ => {
            ui.label("AI 助手：（占位）");
        }
    }
}

/// Files 面板：本地文件列表，可拖到终端上传。
/// "Open…" / "Save…" 调用平台原生文件对话框（PlatformAdapter::FileDialog）。
/// Files 面板：本地 / 远程双栏浏览（功能清单 3.1/3.2 子集）+ 传输队列（3.4）。
fn show_files_pane(ui: &mut egui::Ui, wb: &mut Workbench) {
    // 轮询传输进度。
    crate::files_pane::poll_transfers(&wb.remote_fs);

    let upload = ui.columns(2, |cols| {
        // 本地栏。
        cols[0].heading("本地");
        let up = show_local_pane(&mut cols[0], wb);
        // 远程栏。
        cols[1].heading("远程");
        let dl = show_remote_pane(&mut cols[1], &wb.remote_fs);
        (up, dl)
    });

    // 上传：右键本地文件 → 远程当前目录。
    if let Some((local, remote)) = upload.0 {
        crate::files_pane::start_transfer(&wb.remote_fs, ScpDirection::Upload, local, remote);
    }
    // 下载：右键远程文件 → 本地当前目录。
    if let Some(remote) = upload.1 {
        let name = remote.rsplit('/').next().unwrap_or("file").to_owned();
        let local = wb.local_browser.cwd.join(name).to_string_lossy().into_owned();
        crate::files_pane::start_transfer(&wb.remote_fs, ScpDirection::Download, local, remote);
    }

    // 传输队列。
    ui.add_space(6.0);
    ui.separator();
    ui.heading("传输");
    let mut cancel: Option<String> = None;
    {
        let s = wb.remote_fs.lock().unwrap();
        for t in &s.transfers {
            ui.horizontal(|ui| {
                ui.label(&t.direction);
                ui.label(&t.name);
                match t.status.as_str() {
                    "running" => {
                        ui.add(egui::ProgressBar::new(t.percent()).desired_width(80.0));
                        ui.label(format!("{}/{}", t.bytes_done, t.bytes_total));
                        if ui.small_button("取消").clicked() {
                            cancel = Some(t.job_id.clone());
                        }
                    }
                    "failed" => {
                        ui.colored_label(
                            egui::Color32::from_rgb(220, 90, 90),
                            format!("失败: {}", t.error.as_deref().unwrap_or("")),
                        );
                    }
                    _ => {
                        ui.label("✓ 完成");
                    }
                }
            });
        }
    }
    if let Some(job) = cancel {
        let _ = stacio_core_bridge::CoreHandle::new().cancel_scp_transfer(&job);
    }
}

/// 本地真实文件浏览器（std::fs）。返回 (本地路径, 远程路径) 上传动作。
fn show_local_pane(ui: &mut egui::Ui, wb: &mut Workbench) -> Option<(String, String)> {
    // 原生文件对话框按钮。
    ui.horizontal(|ui| {
        if ui.small_button("打开…").clicked() {
            let adapter = stacio_platform::default_adapter();
            if let Some(path) = adapter.pick_file("选择要上传的文件") {
                if let Some(name) = std::path::Path::new(&path).file_name() {
                    wb.uploads.push(format!("picked → {}", name.to_string_lossy()));
                }
            }
        }
        if ui.small_button("保存…").clicked() {
            let adapter = stacio_platform::default_adapter();
            if let Some(path) = adapter.save_file("另存为", "未命名.txt") {
                if let Some(name) = std::path::Path::new(&path).file_name() {
                    wb.uploads.push(format!("saved → {}", name.to_string_lossy()));
                }
            }
        }
    });
    ui.small(wb.local_browser.cwd.display().to_string());
    ui.separator();

    let mut upload: Option<(String, String)> = None;
    egui::ScrollArea::vertical().show(ui, |ui| {
        // 返回上级。
        if ui.small_button("⬆ ..").clicked() {
            wb.local_browser.go_up();
        }
        let mut enter = None;
        let mut to_upload = None;
        for e in &wb.local_browser.entries {
            let prefix = if e.is_dir { "📁" } else { "📄" };
            let resp = ui.add(egui::Label::new(format!("{prefix} {}", e.name)).selectable(true));
            if resp.double_clicked() && e.is_dir {
                enter = Some(e.name.clone());
            }
            if resp.drag_started() {
                egui::DragAndDrop::set_payload(ui.ctx(), FilePayload { name: e.name.clone() });
            }
            if !e.is_dir {
                let entry_name = e.name.clone();
                resp.context_menu(|ui| {
                    if ui.button("上传到远程…").clicked() {
                        let remote_ready = wb.remote_fs.lock().unwrap().fingerprint.is_some();
                        if remote_ready {
                            let local = wb.local_browser.cwd.join(&entry_name);
                            let remote = {
                                let s = wb.remote_fs.lock().unwrap();
                                let sep = if s.cwd.ends_with('/') { "" } else { "/" };
                                format!("{}{}{}", s.cwd, sep, entry_name)
                            };
                            to_upload = Some((local.to_string_lossy().into_owned(), remote));
                        }
                        ui.close();
                    }
                });
            }
            resp.on_hover_text(if e.is_dir {
                "双击打开".to_string()
            } else {
                format!("{} bytes · drag onto terminal to upload", e.size)
            });
        }
        if let Some(name) = enter {
            wb.local_browser.enter(&name);
        }
        upload = to_upload;
    });
    upload
}

/// SFTP 远程浏览：连接表单 → 指纹确认 → 列目录 / 导航。返回要下载的远程文件路径。
fn show_remote_pane(ui: &mut egui::Ui, state: &Arc<Mutex<crate::files_pane::RemoteFsState>>) -> Option<String> {
    use crate::files_pane::{begin_connect, confirm_host_key, navigate, RemoteFsPhase};
    use stacio_core_bridge::RemoteFileKind;

    let mut connect = false;
    let mut confirm = false;
    let mut cancel_confirm = false;
    let mut nav: Option<String> = None;
    let mut back = false;
    let mut download: Option<String> = None;

    {
        let mut st = state.lock().unwrap();
        match &st.phase {
            RemoteFsPhase::Auth => {
                if ui.button("连接 SFTP").clicked() {
                    connect = true;
                }
                ui.horizontal(|ui| {
                    ui.label("主机");
                    ui.text_edit_singleline(&mut st.host);
                });
                ui.horizontal(|ui| {
                    ui.label("端口");
                    ui.add(egui::DragValue::new(&mut st.port).range(1..=65535));
                });
                ui.horizontal(|ui| {
                    ui.label("用户");
                    ui.text_edit_singleline(&mut st.username);
                });
                ui.horizontal(|ui| {
                    ui.label("密码");
                    ui.add(egui::TextEdit::singleline(&mut st.password).password(true));
                });
                ui.checkbox(&mut st.use_agent, "Agent");
            }
            RemoteFsPhase::Busy(msg) => {
                ui.label(format!("⏳ {msg}"));
            }
            RemoteFsPhase::ConfirmHostKey {
                fingerprint,
                previous,
                ..
            } => {
                if previous.is_some() {
                    ui.colored_label(egui::Color32::from_rgb(220, 90, 90), "⚠ 主机密钥已变更");
                } else {
                    ui.label("首次连接，确认指纹：");
                }
                ui.label(format!("SHA256: {fingerprint}"));
                ui.horizontal(|ui| {
                    if ui.button("信任并连接").clicked() {
                        confirm = true;
                    }
                    if ui.button("取消").clicked() {
                        cancel_confirm = true;
                    }
                });
            }
            RemoteFsPhase::Ready => {
                ui.small(&st.cwd);
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    if ui.small_button("⬆ ..").clicked() {
                        nav = Some("..".to_string());
                    }
                    for e in &st.entries {
                        let prefix = match e.kind {
                            RemoteFileKind::Directory => "📁",
                            RemoteFileKind::Symlink => "🔗",
                            RemoteFileKind::File => "📄",
                        };
                        let resp =
                            ui.add(egui::Label::new(format!("{prefix} {}", e.path)).selectable(true));
                        if resp.double_clicked() && e.kind == RemoteFileKind::Directory {
                            nav = Some(e.path.clone());
                        }
                        if e.kind == RemoteFileKind::File {
                            let path = e.path.clone();
                            resp.context_menu(|ui| {
                                if ui.button("下载到本地…").clicked() {
                                    download = Some(path);
                                    ui.close();
                                }
                            });
                        }
                        resp.on_hover_text(if e.kind == RemoteFileKind::Directory {
                            "双击打开".to_string()
                        } else {
                            format!("{} bytes", e.size)
                        });
                    }
                });
            }
            RemoteFsPhase::Failed(msg) => {
                ui.colored_label(egui::Color32::from_rgb(220, 90, 90), msg);
                if ui.button("返回").clicked() {
                    back = true;
                }
            }
        }
    }

    // 释放锁后再触发动作（避免跨阻塞调用持锁）。
    if connect {
        begin_connect(state);
    }
    if confirm {
        confirm_host_key(state);
    }
    if cancel_confirm {
        state.lock().unwrap().phase = RemoteFsPhase::Auth;
    }
    if let Some(n) = nav {
        if state.lock().unwrap().fingerprint.is_some() {
            navigate(state, &n);
        }
    }
    if back {
        state.lock().unwrap().phase = RemoteFsPhase::Auth;
    }
    download
}

fn show_logs_pane(ui: &mut egui::Ui, wb: &mut Workbench) {
    ui.heading("诊断");
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.label("工作台：三栏布局已激活");
        ui.label(format!("tabs: {}", wb.tabs.len()));
        ui.label(format!("uploads: {}", wb.uploads.len()));
    });
}
