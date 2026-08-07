//! Files 面板（功能清单 3.1/3.2 子集）：本地真实浏览器 + SFTP 远程浏览。
//!
//! SFTP 调用是无状态的（每次传 config+secret+fingerprint 独立建连），
//! 因此本面板自带连接表单与指纹确认，浏览时逐个目录重新建连（PoC 简化）。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use stacio_core_bridge::{
    CoreHandle, FtpAuthSecret, FtpConnectionConfig, HostKeyTrustDecision, RemoteFileEntry,
    ScpDirection, ScpTransferJob, SshAuthMethod, SshAuthSecret, SshConnectionConfig,
    SshRuntimeError,
};

/// 远程协议（功能清单 3.1/3.10）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsProtocol {
    Sftp,
    Ftp,
}

// ---------------------------------------------------------------------------
// 本地浏览器
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct LocalEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
}

pub struct LocalBrowser {
    pub cwd: PathBuf,
    pub entries: Vec<LocalEntry>,
}

impl LocalBrowser {
    pub fn new() -> Self {
        let cwd = std::env::var("HOME")
            .map(PathBuf::from)
            .or_else(|_| std::env::current_dir())
            .unwrap_or_else(|_| PathBuf::from("/"));
        let mut b = Self { cwd, entries: Vec::new() };
        b.refresh();
        b
    }

    fn refresh(&mut self) {
        self.entries = std::fs::read_dir(&self.cwd)
            .map(|rd| {
                let mut v: Vec<LocalEntry> = rd
                    .flatten()
                    .filter_map(|e| {
                        let name = e.file_name().to_string_lossy().into_owned();
                        let md = e.metadata().ok()?;
                        Some(LocalEntry {
                            name,
                            is_dir: md.is_dir(),
                            size: md.len(),
                        })
                    })
                    .collect();
                v.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
                v
            })
            .unwrap_or_default();
    }

    pub fn enter(&mut self, name: &str) {
        self.cwd.push(name);
        self.refresh();
    }

    pub fn go_up(&mut self) {
        if self.cwd.pop() {
            self.refresh();
        }
    }
}

impl Default for LocalBrowser {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// SFTP 远程浏览
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum RemoteFsPhase {
    /// 连接表单。
    Auth,
    /// 探测 / 列目录中。
    Busy(String),
    /// 等待用户确认主机指纹（previous = Some(旧指纹) 表示已变更）。
    ConfirmHostKey {
        fingerprint: String,
        raw_key: Vec<u8>,
        previous: Option<String>,
    },
    /// 已连接，可浏览。
    Ready,
    Failed(String),
}

pub struct RemoteFsState {
    /// 协议（SFTP / FTP）。
    pub protocol: FsProtocol,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub use_agent: bool,
    pub phase: RemoteFsPhase,
    /// 已确认的主机指纹（连接后用于每次 SFTP 调用）。
    pub fingerprint: Option<String>,
    pub cwd: String,
    pub entries: Vec<RemoteFileEntry>,
    /// 进行中 / 已完成的传输（P4-5）。
    pub transfers: Vec<TransferEntry>,
    /// 传输冲突策略（功能清单 3.6）：ask / keepBoth / overwrite / rename / skip。
    pub conflict_policy: String,
}

/// 一条传输记录（UI 队列展示 + 进度轮询）。
#[derive(Debug, Clone)]
pub struct TransferEntry {
    pub job_id: String,
    pub name: String,
    pub direction: String,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub status: String,
    pub error: Option<String>,
}

impl TransferEntry {
    pub fn percent(&self) -> f32 {
        if self.bytes_total == 0 {
            0.0
        } else {
            (self.bytes_done as f32 / self.bytes_total as f32).clamp(0.0, 1.0)
        }
    }
}

impl RemoteFsState {
    pub fn new() -> Self {
        Self {
            protocol: FsProtocol::Sftp,
            host: String::new(),
            port: 22,
            username: String::new(),
            password: String::new(),
            use_agent: false,
            phase: RemoteFsPhase::Auth,
            fingerprint: None,
            cwd: "/".to_owned(),
            entries: Vec::new(),
            transfers: Vec::new(),
            conflict_policy: "ask".to_owned(),
        }
    }

    fn config(&self) -> SshConnectionConfig {
        SshConnectionConfig {
            host: self.host.clone(),
            port: self.port,
            username: self.username.clone(),
            auth_method: if self.use_agent {
                SshAuthMethod::Agent
            } else {
                SshAuthMethod::Password {
                    credential_ref: String::new(),
                }
            },
            connect_timeout_ms: 10_000,
        }
    }

    fn secret(&self) -> Option<SshAuthSecret> {
        if self.use_agent {
            Some(SshAuthSecret::Agent)
        } else if self.password.is_empty() {
            None
        } else {
            Some(SshAuthSecret::Password {
                value: self.password.clone(),
            })
        }
    }
}

impl Default for RemoteFsState {
    fn default() -> Self {
        Self::new()
    }
}

/// 在后台线程开始连接：SFTP 探测密钥 + Reject 决策探针；FTP 直接连接（无 host key）。
pub fn begin_connect(state: &Arc<Mutex<RemoteFsState>>) {
    let handle = CoreHandle::new();
    let st = state.clone();
    std::thread::spawn(move || {
        let (config, _secret, protocol) = {
            let s = st.lock().unwrap_or_else(|e| e.into_inner());
            (s.config(), s.secret(), s.protocol)
        };
        // FTP：无 host key，直接就绪。
        if protocol == FsProtocol::Ftp {
            finish_connect(&st, String::new());
            return;
        }
        let observed = match handle.probe_host_key(config.clone()) {
            Ok(k) => k,
            Err(e) => {
                st.lock().unwrap_or_else(|e| e.into_inner()).phase = RemoteFsPhase::Failed(format!("探测主机密钥失败: {e}"));
                return;
            }
        };
        let fingerprint = observed.fingerprint_sha256.clone();
        match handle.apply_host_key_decision(
            &config.host,
            config.port,
            observed.raw_key.clone(),
            HostKeyTrustDecision::Reject,
        ) {
            Ok(_) => finish_connect(&st, fingerprint),
            Err(SshRuntimeError::UnknownHostKey) => {
                st.lock().unwrap_or_else(|e| e.into_inner()).phase = RemoteFsPhase::ConfirmHostKey {
                    fingerprint,
                    raw_key: observed.raw_key,
                    previous: None,
                };
            }
            Err(SshRuntimeError::HostKeyChanged { previous_fingerprint_sha256 }) => {
                st.lock().unwrap_or_else(|e| e.into_inner()).phase = RemoteFsPhase::ConfirmHostKey {
                    fingerprint,
                    raw_key: observed.raw_key,
                    previous: Some(previous_fingerprint_sha256),
                };
            }
            Err(e) => {
                st.lock().unwrap_or_else(|e| e.into_inner()).phase = RemoteFsPhase::Failed(format!("主机密钥校验失败: {e}"));
            }
        }
    });
}

/// 用户确认指纹后：应用信任决策并列出初始目录。
pub fn confirm_host_key(state: &Arc<Mutex<RemoteFsState>>) {
    let handle = CoreHandle::new();
    let st = state.clone();
    std::thread::spawn(move || {
        let (host, port, fingerprint, raw_key, previous) = {
            let s = st.lock().unwrap_or_else(|e| e.into_inner());
            match &s.phase {
                RemoteFsPhase::ConfirmHostKey {
                    fingerprint,
                    raw_key,
                    previous,
                } => (
                    s.host.clone(),
                    s.port,
                    fingerprint.clone(),
                    raw_key.clone(),
                    previous.clone(),
                ),
                _ => return,
            }
        };
        let decision = match previous {
            Some(old) => HostKeyTrustDecision::TrustAndReplace {
                previous_fingerprint_sha256: old,
            },
            None => HostKeyTrustDecision::TrustAndSave,
        };
        match handle.apply_host_key_decision(&host, port, raw_key, decision) {
            Ok(_) => finish_connect(&st, fingerprint),
            Err(e) => {
                st.lock().unwrap_or_else(|e| e.into_inner()).phase = RemoteFsPhase::Failed(format!("主机密钥确认失败: {e}"));
            }
        }
    });
}

/// 记录指纹并列出初始目录（/ 或用户主目录）。
fn finish_connect(state: &Arc<Mutex<RemoteFsState>>, fingerprint: String) {
    let st = state.clone();
    list_dir(&st, fingerprint, "/".to_owned());
}

/// 后台列出远程目录（按协议分支）。
pub fn list_dir(state: &Arc<Mutex<RemoteFsState>>, fingerprint: String, path: String) {
    let handle = CoreHandle::new();
    let st = state.clone();
    std::thread::spawn(move || {
        let (config, secret, protocol) = {
            let s = st.lock().unwrap_or_else(|e| e.into_inner());
            (s.config(), s.secret(), s.protocol)
        };
        let Some(secret) = secret else {
            st.lock().unwrap_or_else(|e| e.into_inner()).phase = RemoteFsPhase::Failed("未提供认证信息".to_owned());
            return;
        };
        st.lock().unwrap_or_else(|e| e.into_inner()).phase = RemoteFsPhase::Busy(format!("列目录 {path}"));
        let result = match protocol {
            FsProtocol::Sftp => {
                handle.list_sftp_directory(config, secret, &fingerprint, &path)
            }
            FsProtocol::Ftp => {
                let ftp_config = FtpConnectionConfig {
                    host: ftp_host(&config),
                    port: config.port,
                    username: config.username,
                    connect_timeout_ms: 10_000,
                };
                let ftp_secret = ftp_secret(secret);
                handle.list_ftp_directory(ftp_config, ftp_secret, &path)
            }
        };
        match result {
            Ok(entries) => {
                let mut s = st.lock().unwrap_or_else(|e| e.into_inner());
                s.entries = entries;
                s.cwd = path;
                s.fingerprint = Some(fingerprint);
                s.phase = RemoteFsPhase::Ready;
            }
            Err(e) => {
                st.lock().unwrap_or_else(|e| e.into_inner()).phase = RemoteFsPhase::Failed(format!("列目录失败: {e}"));
            }
        }
    });
}

fn ftp_host(config: &SshConnectionConfig) -> String {
    config.host.clone()
}

fn ftp_secret(secret: SshAuthSecret) -> FtpAuthSecret {
    match secret {
        SshAuthSecret::Password { value } => FtpAuthSecret::Password { value },
        _ => FtpAuthSecret::Anonymous,
    }
}

/// 进入子目录（或返回上级）。
pub fn navigate(state: &Arc<Mutex<RemoteFsState>>, name: &str) {
    let (fingerprint, next) = {
        let s = state.lock().unwrap_or_else(|e| e.into_inner());
        let fp = s.fingerprint.clone();
        let next = if name == ".." {
            let mut p = s.cwd.clone();
            if p == "/" {
                p
            } else {
                p = p.rsplit_once('/').map(|(h, _)| if h.is_empty() { "/".to_owned() } else { h.to_owned() }).unwrap_or("/".to_owned());
                p
            }
        } else {
            let sep = if s.cwd.ends_with('/') { "" } else { "/" };
            format!("{}{}{}", s.cwd, sep, name)
        };
        (fp, next)
    };
    if let Some(fp) = fingerprint {
        list_dir(state, fp, next);
    }
}

// ---------------------------------------------------------------------------
// 文件传输（P4-5）
// ---------------------------------------------------------------------------

fn update_entry(state: &Arc<Mutex<RemoteFsState>>, job_id: &str, f: impl FnOnce(&mut TransferEntry)) {
    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(e) = s.transfers.iter_mut().find(|t| t.job_id == job_id) {
        f(e);
    }
}

/// 启动 SCP 传输（后台线程阻塞执行；进度推入 core 全局 registry，UI 轮询）。
pub fn start_transfer(
    state: &Arc<Mutex<RemoteFsState>>,
    direction: ScpDirection,
    local_path: String,
    remote_path: String,
) {
    let (config, secret, fingerprint) = {
        let s = state.lock().unwrap_or_else(|e| e.into_inner());
        (s.config(), s.secret(), s.fingerprint.clone())
    };
    let (Some(secret), Some(fp)) = (secret, fingerprint) else { return };

    let job_id = format!(
        "job-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    );
    let name = local_path
        .rsplit('/')
        .next()
        .map(str::to_owned)
        .or_else(|| remote_path.rsplit('/').next().map(str::to_owned))
        .unwrap_or_else(|| "file".to_owned());
    let bytes_total = std::fs::metadata(&local_path).map(|m| m.len()).unwrap_or(0);
    {
        let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
        s.transfers.push(TransferEntry {
            job_id: job_id.clone(),
            name: name.clone(),
            direction: match direction {
                ScpDirection::Upload => "↑ 上传".to_owned(),
                ScpDirection::Download => "↓ 下载".to_owned(),
            },
            bytes_done: 0,
            bytes_total,
            status: "running".to_owned(),
            error: None,
        });
    }

    let st = state.clone();
    std::thread::spawn(move || {
        let job = ScpTransferJob {
            id: job_id.clone(),
            direction,
            source_path: local_path,
            destination_path: remote_path,
            bytes_total,
        };
        let result = {
            let protocol = st.lock().unwrap_or_else(|e| e.into_inner()).protocol;
            match protocol {
                FsProtocol::Sftp => CoreHandle::new().run_scp_transfer(config, secret, &fp, job),
                FsProtocol::Ftp => {
                    let ftp_config = FtpConnectionConfig {
                        host: config.host,
                        port: config.port,
                        username: config.username,
                        connect_timeout_ms: 10_000,
                    };
                    CoreHandle::new().run_ftp_transfer(ftp_config, ftp_secret(secret), job)
                }
            }
        };
        match result {
            Ok(progress) => {
                let last = progress.last().cloned();
                update_entry(&st, &job_id, |e| {
                    if let Some(p) = last {
                        e.bytes_done = p.bytes_done;
                        if p.bytes_total > 0 {
                            e.bytes_total = p.bytes_total;
                        }
                    }
                    e.status = "completed".to_owned();
                });
            }
            Err(err) => {
                update_entry(&st, &job_id, |e| {
                    e.status = "failed".to_owned();
                    e.error = Some(err.to_string());
                });
            }
        }
    });
}

/// 轮询所有进行中传输的进度（每帧调用）。
pub fn poll_transfers(state: &Arc<Mutex<RemoteFsState>>) {
    let handle = CoreHandle::new();
    let running: Vec<String> = state
        .lock()
        .unwrap()
        .transfers
        .iter()
        .filter(|t| t.status == "running")
        .map(|t| t.job_id.clone())
        .collect();
    for job_id in running {
        if let Ok(events) = handle.take_scp_progress(&job_id) {
            if let Some(last) = events.last() {
                let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(e) = s.transfers.iter_mut().find(|t| t.job_id == job_id) {
                    e.bytes_done = last.bytes_done;
                    if last.bytes_total > 0 {
                        e.bytes_total = last.bytes_total;
                    }
                    if last.status == "completed" || last.status == "done" {
                        e.status = "completed".to_owned();
                    }
                }
            }
        }
    }
}
