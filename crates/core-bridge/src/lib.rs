//! UI 面对接层：封装共享 Rust Core（stacio_core）。
//!
//! 对应 `00-platform-architecture.md` §5：Linux 直接 crate 依赖（同语言）；
//! Windows UI 同为 Rust（egui 自绘），同样直接依赖，无需 UniFFI。
//!
//! 职责：
//! - 计算平台数据库路径（`00-platform-architecture.md` §4.3，`STACIO_DB` 可覆盖）
//! - 暴露 UI 需要的 Core 能力（薄封装，不重写业务逻辑）

use std::path::PathBuf;

pub use stacio_core::domain::session::{
    SessionDraft, SessionError, SessionFolder, SessionRecord, SessionSidebarSnapshot,
};

/// Core 句柄：持有数据库路径。
pub struct CoreHandle {
    db_path: PathBuf,
}

impl CoreHandle {
    /// 使用平台默认数据库路径创建句柄（`STACIO_DB` 环境变量可覆盖）。
    pub fn new() -> Self {
        let db_path = std::env::var("STACIO_DB")
            .map(PathBuf::from)
            .unwrap_or_else(|_| default_db_path());
        Self { db_path }
    }

    /// 当前数据库路径（调试/展示用）。
    pub fn db_path(&self) -> &str {
        self.db_path.to_str().unwrap_or_default()
    }

    /// 健康检查：证明核心库已加载。
    pub fn health(&self) -> stacio_core::CoreHealth {
        stacio_core::health()
    }

    /// 侧栏快照：一次取侧栏所需全部数据（文件夹 + 会话 + 排序 + 图标）。
    pub fn session_sidebar_snapshot(&self) -> Result<SessionSidebarSnapshot, SessionError> {
        stacio_core::load_session_sidebar_snapshot(self.db_str())
    }

    /// 创建会话文件夹。
    pub fn create_folder(
        &self,
        parent_id: Option<&str>,
        name: &str,
    ) -> Result<SessionFolder, SessionError> {
        stacio_core::create_session_folder(self.db_str(), parent_id.map(str::to_owned), name.to_owned())
    }

    /// 创建会话记录。
    pub fn create_session(&self, draft: SessionDraft) -> Result<SessionRecord, SessionError> {
        stacio_core::create_session_record(self.db_str(), draft)
    }

    /// 删除会话记录（同时清理 known_host）。
    pub fn delete_session(&self, id: &str) -> Result<(), SessionError> {
        stacio_core::delete_session_record(self.db_str(), id.to_owned())
    }

    fn db_str(&self) -> String {
        self.db_path.to_string_lossy().into_owned()
    }
}

impl Default for CoreHandle {
    fn default() -> Self {
        Self::new()
    }
}

/// 平台默认数据库路径（per `00-platform-architecture.md` §4.3）。
/// macOS 开发用独立 `StacioCross` 目录，避免触碰真机 Stacio 数据；
/// Windows / Linux 按生产约定路径。
fn default_db_path() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_owned());
        PathBuf::from(home).join("Library/Application Support/StacioCross/stacio.db")
    }
    #[cfg(target_os = "windows")]
    {
        let local = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".to_owned());
        PathBuf::from(local).join("Stacio/stacio.db")
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
            PathBuf::from(xdg).join("stacio/stacio.db")
        } else {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_owned());
            PathBuf::from(home).join(".local/share/stacio/stacio.db")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stacio_core::domain::session::SessionDraft;

    /// 端到端：临时 SQLite 库 → 建文件夹 + 会话 → 快照校验。
    /// 走真实 stacio_core 迁移（0001~0005）与 DTO，证明集成链可用。
    #[test]
    fn session_roundtrip_through_core() {
        let dir = std::env::temp_dir().join(format!("stacio-core-bridge-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("test.db");
        let db_str = db.to_string_lossy().into_owned();

        let folder = stacio_core::create_session_folder(db_str.clone(), None, "Production".to_owned())
            .expect("create folder");
        let session = stacio_core::create_session_record(
            db_str.clone(),
            SessionDraft {
                folder_id: Some(folder.id.clone()),
                name: "web-01".to_owned(),
                protocol: "ssh".to_owned(),
                host: "10.0.1.10".to_owned(),
                port: 22,
                username: Some("root".to_owned()),
                private_key_path: None,
                credential_id: None,
                tags: vec![],
                config_json: None,
            },
        )
        .expect("create session");

        let snap = stacio_core::load_session_sidebar_snapshot(db_str).expect("load snapshot");
        assert_eq!(snap.folders.len(), 1);
        assert_eq!(snap.folders[0].name, "Production");
        assert_eq!(snap.sessions.len(), 1);
        assert_eq!(snap.sessions[0].id, session.id);
        assert_eq!(snap.sessions[0].host, "10.0.1.10");
        assert_eq!(snap.sessions[0].folder_id.as_deref(), Some(folder.id.as_str()));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
