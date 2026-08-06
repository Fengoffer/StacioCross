//! UI 面对接层：封装共享 Rust Core（stacio_core）。
//!
//! 对应 `00-platform-architecture.md` §5：Linux 直接 crate 依赖（同语言）；
//! Windows UI 同为 Rust（egui 自绘），同样直接依赖，无需 UniFFI。
//!
//! 职责：
//! - 计算平台数据库路径（`00-platform-architecture.md` §4.3，`STACIO_DB` 可覆盖）
//! - 暴露 UI 需要的 Core 能力（薄封装，不重写业务逻辑）

use std::path::PathBuf;

pub use stacio_core::domain::files::{RemoteFileEntry, RemoteFileKind};
pub use stacio_core::domain::scp::{
    ScpDirection, ScpResumeOptions, ScpTransferJob, ScpTransferProgress,
};
pub use stacio_core::domain::serial::SerialConnectionConfig;
pub use stacio_core::domain::session::{
    QuickConnectTarget, SessionDraft, SessionError, SessionFolder, SessionRecord,
    SessionSidebarSnapshot, SessionUpdate,
};
pub use stacio_core::domain::telnet::TelnetConnectionConfig;
pub use stacio_core::domain::ssh::{
    HostKeyTrustDecision, HostKeyVerification, LiveSshHostKey, SshAuthMethod, SshAuthSecret,
    SshConnectionConfig, SshRuntimeError,
};
pub use stacio_core::domain::terminal::{TerminalOutputBatch, TerminalRuntime, TerminalRuntimeError};
pub use stacio_core::services::live_shell_service::LiveShellStatus;

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

    /// 重命名会话文件夹。
    pub fn rename_folder(&self, id: &str, name: &str) -> Result<SessionFolder, SessionError> {
        stacio_core::rename_session_folder(self.db_str(), id.to_owned(), name.to_owned())
    }

    /// 删除会话文件夹。
    pub fn delete_folder(&self, id: &str) -> Result<(), SessionError> {
        stacio_core::delete_session_folder(self.db_str(), id.to_owned())
    }

    /// 创建会话记录。
    pub fn create_session(&self, draft: SessionDraft) -> Result<SessionRecord, SessionError> {
        stacio_core::create_session_record(self.db_str(), draft)
    }

    /// 部分更新会话记录。
    pub fn update_session(&self, id: &str, update: SessionUpdate) -> Result<SessionRecord, SessionError> {
        stacio_core::update_session_record(self.db_str(), id.to_owned(), update)
    }

    /// 删除会话记录（同时清理 known_host）。
    pub fn delete_session(&self, id: &str) -> Result<(), SessionError> {
        stacio_core::delete_session_record(self.db_str(), id.to_owned())
    }

    /// 解析 `user@host:port` 快速连接串。
    pub fn parse_quick_connect(&self, input: &str) -> Result<QuickConnectTarget, SessionError> {
        stacio_core::parse_quick_connect(input.to_owned())
    }

    // -----------------------------------------------------------------------
    // 文件传输（P4-4：SFTP 列目录）
    // -----------------------------------------------------------------------

    /// 列出远程 SFTP 目录（每次调用以 config+secret+fingerprint 独立建连）。
    pub fn list_sftp_directory(
        &self,
        config: SshConnectionConfig,
        secret: SshAuthSecret,
        expected_fingerprint_sha256: &str,
        remote_path: &str,
    ) -> Result<Vec<RemoteFileEntry>, SshRuntimeError> {
        stacio_core::list_live_sftp_directory(
            config,
            secret,
            expected_fingerprint_sha256.to_owned(),
            remote_path.to_owned(),
        )
    }

    // -----------------------------------------------------------------------
    // 文件传输（P4-5：SCP 上传/下载，长任务模式）
    // -----------------------------------------------------------------------

    /// 启动 SCP 传输（阻塞执行，进度推入全局 registry；UI 另线程轮询 take_scp_progress）。
    pub fn run_scp_transfer(
        &self,
        config: SshConnectionConfig,
        secret: SshAuthSecret,
        expected_fingerprint_sha256: &str,
        job: ScpTransferJob,
    ) -> Result<Vec<ScpTransferProgress>, SshRuntimeError> {
        stacio_core::run_live_scp_transfer(
            config,
            secret,
            expected_fingerprint_sha256.to_owned(),
            job,
        )
    }

    /// 取出某 job 的进度批次（取走即清空）。
    pub fn take_scp_progress(
        &self,
        job_id: &str,
    ) -> Result<Vec<ScpTransferProgress>, SshRuntimeError> {
        stacio_core::take_live_scp_transfer_progress_batch(job_id.to_owned())
    }

    /// 取消传输。
    pub fn cancel_scp_transfer(&self, job_id: &str) -> Result<bool, SshRuntimeError> {
        stacio_core::cancel_live_scp_transfer(job_id.to_owned())
    }

    // -----------------------------------------------------------------------
    // SSH / 终端运行时（P4-2：首条 SSH 链路）
    // -----------------------------------------------------------------------

    /// 探测主机密钥（连接后立即断开，返回 observed 指纹与原始 key）。
    pub fn probe_host_key(
        &self,
        config: SshConnectionConfig,
    ) -> Result<LiveSshHostKey, SshRuntimeError> {
        stacio_core::probe_live_ssh_host_key(config)
    }

    /// 应用主机密钥信任决策（存入 known_host 库）。
    pub fn apply_host_key_decision(
        &self,
        host: &str,
        port: u16,
        host_key: Vec<u8>,
        decision: HostKeyTrustDecision,
    ) -> Result<HostKeyVerification, SshRuntimeError> {
        stacio_core::apply_host_key_decision_in_database(
            self.db_str(),
            host.to_owned(),
            port,
            host_key,
            decision,
        )
    }

    /// 启动 SSH live shell（长任务：返回后由 UI 轮询输出/状态）。
    pub fn start_ssh_shell(
        &self,
        config: SshConnectionConfig,
        secret: SshAuthSecret,
        expected_fingerprint_sha256: String,
        cols: u32,
        rows: u32,
    ) -> Result<LiveShellStatus, SshRuntimeError> {
        stacio_core::start_live_ssh_shell_runtime(config, secret, expected_fingerprint_sha256, cols, rows)
    }

    /// 启动 Telnet live shell（无 host key / 无认证）。
    pub fn start_telnet_shell(
        &self,
        config: TelnetConnectionConfig,
        cols: u32,
        rows: u32,
    ) -> Result<LiveShellStatus, SshRuntimeError> {
        stacio_core::start_live_telnet_shell_runtime(config, cols, rows)
    }

    /// 启动串口 live shell（无 host key / 无认证）。
    pub fn start_serial_shell(
        &self,
        config: SerialConnectionConfig,
        cols: u32,
        rows: u32,
    ) -> Result<LiveShellStatus, SshRuntimeError> {
        stacio_core::start_live_serial_shell_runtime(config, cols, rows)
    }

    /// 轮询 live shell 状态。
    pub fn poll_ssh_shell(&self, runtime_id: &str) -> Result<LiveShellStatus, TerminalRuntimeError> {
        stacio_core::poll_live_ssh_shell(runtime_id.to_owned())
    }

    /// 取出待显示的输出批次（远端 → UI）。
    pub fn take_output(&self, runtime_id: &str) -> Result<TerminalOutputBatch, TerminalRuntimeError> {
        stacio_core::take_terminal_output_batch(runtime_id.to_owned())
    }

    /// 写入用户输入（UI → 远端，pump 负责发送）。
    pub fn write_input(&self, runtime_id: &str, bytes: Vec<u8>) -> Result<(), TerminalRuntimeError> {
        stacio_core::write_terminal_input(runtime_id.to_owned(), bytes)
    }

    /// 记录终端尺寸变更（联动 live shell 的 PTY resize）。
    pub fn record_resize(
        &self,
        runtime_id: &str,
        cols: u32,
        rows: u32,
    ) -> Result<TerminalRuntime, TerminalRuntimeError> {
        stacio_core::record_terminal_resize(runtime_id.to_owned(), cols, rows)
    }

    /// 关闭终端运行时。
    pub fn close_runtime(&self, runtime_id: &str) -> Result<TerminalRuntime, TerminalRuntimeError> {
        stacio_core::close_terminal_runtime(runtime_id.to_owned())
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

    /// 主机密钥决策流程（SSH 首连确认的核心逻辑）：
    /// 未知 → Reject 探针报 UnknownHostKey → TrustAndSave → 之后 Reject 返回 Trusted。
    #[test]
    fn host_key_decision_probe_confirm_flow() {
        let dir = std::env::temp_dir().join(format!("stacio-core-bridge-hostkey-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("hostkey.db");
        // 先设置 STACIO_DB 再创建 handle（CoreHandle::new 构造时读取）。
        std::env::set_var("STACIO_DB", db.to_string_lossy().into_owned());
        let handle = CoreHandle::new();

        let fake_key = b"fake-host-key-bytes".to_vec();

        // 首次：未知主机 → Reject 探针应报 UnknownHostKey。
        let probe = handle.apply_host_key_decision("example.com", 22, fake_key.clone(), HostKeyTrustDecision::Reject);
        assert!(matches!(probe, Err(SshRuntimeError::UnknownHostKey)));

        // 用户确认 → TrustAndSave → Trusted。
        let save = handle.apply_host_key_decision("example.com", 22, fake_key.clone(), HostKeyTrustDecision::TrustAndSave);
        assert!(save.is_ok());

        // 之后 Reject 探针 → Trusted（已保存且匹配）。
        let again = handle.apply_host_key_decision("example.com", 22, fake_key, HostKeyTrustDecision::Reject);
        assert!(again.is_ok());

        std::env::remove_var("STACIO_DB");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Quick Connect 解析：`user@host:port` → 目标。
    #[test]
    fn quick_connect_parses() {
        let handle = CoreHandle::new();
        let target = handle.parse_quick_connect("root@example.com:2222").expect("parse");
        assert_eq!(target.protocol, "ssh");
        assert_eq!(target.username.as_deref(), Some("root"));
        assert_eq!(target.host, "example.com");
        assert_eq!(target.port, 2222);

        let bare = handle.parse_quick_connect("10.0.1.5").expect("parse bare host");
        assert_eq!(bare.username, None);
        assert_eq!(bare.port, 22);
    }
}
