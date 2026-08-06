//! Files 面板（功能清单 3.1/3.2 子集）：本地真实浏览器 + SFTP 远程浏览。
//!
//! SFTP 调用是无状态的（每次传 config+secret+fingerprint 独立建连），
//! 因此本面板自带连接表单与指纹确认，浏览时逐个目录重新建连（PoC 简化）。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use stacio_core_bridge::{
    CoreHandle, HostKeyTrustDecision, RemoteFileEntry, SshAuthMethod, SshAuthSecret,
    SshConnectionConfig, SshRuntimeError,
};

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
}

impl RemoteFsState {
    pub fn new() -> Self {
        Self {
            host: String::new(),
            port: 22,
            username: String::new(),
            password: String::new(),
            use_agent: false,
            phase: RemoteFsPhase::Auth,
            fingerprint: None,
            cwd: "/".to_owned(),
            entries: Vec::new(),
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

/// 在后台线程开始连接：探测密钥 + Reject 决策探针。
pub fn begin_connect(state: &Arc<Mutex<RemoteFsState>>) {
    let handle = CoreHandle::new();
    let st = state.clone();
    std::thread::spawn(move || {
        let (config, _secret) = {
            let s = st.lock().unwrap();
            (s.config(), s.secret())
        };
        let observed = match handle.probe_host_key(config.clone()) {
            Ok(k) => k,
            Err(e) => {
                st.lock().unwrap().phase = RemoteFsPhase::Failed(format!("探测主机密钥失败: {e}"));
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
                st.lock().unwrap().phase = RemoteFsPhase::ConfirmHostKey {
                    fingerprint,
                    raw_key: observed.raw_key,
                    previous: None,
                };
            }
            Err(SshRuntimeError::HostKeyChanged { previous_fingerprint_sha256 }) => {
                st.lock().unwrap().phase = RemoteFsPhase::ConfirmHostKey {
                    fingerprint,
                    raw_key: observed.raw_key,
                    previous: Some(previous_fingerprint_sha256),
                };
            }
            Err(e) => {
                st.lock().unwrap().phase = RemoteFsPhase::Failed(format!("主机密钥校验失败: {e}"));
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
            let s = st.lock().unwrap();
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
                st.lock().unwrap().phase = RemoteFsPhase::Failed(format!("主机密钥确认失败: {e}"));
            }
        }
    });
}

/// 记录指纹并列出初始目录（/ 或用户主目录）。
fn finish_connect(state: &Arc<Mutex<RemoteFsState>>, fingerprint: String) {
    let st = state.clone();
    list_dir(&st, fingerprint, "/".to_owned());
}

/// 后台列出远程目录。
pub fn list_dir(state: &Arc<Mutex<RemoteFsState>>, fingerprint: String, path: String) {
    let handle = CoreHandle::new();
    let st = state.clone();
    std::thread::spawn(move || {
        let (config, secret) = {
            let s = st.lock().unwrap();
            (s.config(), s.secret())
        };
        let Some(secret) = secret else {
            st.lock().unwrap().phase = RemoteFsPhase::Failed("未提供认证信息".to_owned());
            return;
        };
        st.lock().unwrap().phase = RemoteFsPhase::Busy(format!("列目录 {path}"));
        match handle.list_sftp_directory(config, secret, &fingerprint, &path) {
            Ok(entries) => {
                let mut s = st.lock().unwrap();
                s.entries = entries;
                s.cwd = path;
                s.fingerprint = Some(fingerprint);
                s.phase = RemoteFsPhase::Ready;
            }
            Err(e) => {
                st.lock().unwrap().phase = RemoteFsPhase::Failed(format!("列目录失败: {e}"));
            }
        }
    });
}

/// 进入子目录（或返回上级）。
pub fn navigate(state: &Arc<Mutex<RemoteFsState>>, name: &str) {
    let (fingerprint, next) = {
        let s = state.lock().unwrap();
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
