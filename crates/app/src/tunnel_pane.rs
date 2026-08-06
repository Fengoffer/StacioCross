//! 隧道面板（功能清单 4.x，License: sshTunnel）。
//!
//! SSH 连接上下文（config+secret+fingerprint）通过探测/确认流程获取；
//! 隧道配置存 SQLite，运行状态走 live tunnel manager（启动→轮询）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use stacio_core_bridge::{
    CoreHandle, HostKeyTrustDecision, SshAuthMethod, SshAuthSecret, SshConnectionConfig,
    SshRuntimeError, TunnelKind, TunnelProfile, TunnelState,
};

#[derive(Debug, Clone)]
pub enum TunnelPhase {
    /// 连接表单（SSH 上下文）。
    Auth,
    Busy(String),
    ConfirmHostKey {
        fingerprint: String,
        raw_key: Vec<u8>,
        previous: Option<String>,
    },
    /// 已连接（指纹就绪），可管理隧道。
    Ready,
    Failed(String),
}

pub struct TunnelPaneState {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub use_agent: bool,
    pub phase: TunnelPhase,
    pub fingerprint: Option<String>,
    pub profiles: Vec<TunnelProfile>,
    pub statuses: HashMap<String, TunnelState>,
    // 新建表单草稿。
    pub draft_kind: TunnelKind,
    pub draft_local_port: u16,
    pub draft_remote_host: String,
    pub draft_remote_port: u16,
}

impl TunnelPaneState {
    pub fn new() -> Self {
        Self {
            host: String::new(),
            port: 22,
            username: String::new(),
            password: String::new(),
            use_agent: false,
            phase: TunnelPhase::Auth,
            fingerprint: None,
            profiles: Vec::new(),
            statuses: HashMap::new(),
            draft_kind: TunnelKind::Local,
            draft_local_port: 8080,
            draft_remote_host: String::new(),
            draft_remote_port: 80,
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

impl Default for TunnelPaneState {
    fn default() -> Self {
        Self::new()
    }
}

/// 开始连接：探测主机密钥 + Reject 决策探针。
pub fn begin_connect(state: &Arc<Mutex<TunnelPaneState>>) {
    let handle = CoreHandle::new();
    let st = state.clone();
    std::thread::spawn(move || {
        let config = { st.lock().unwrap().config() };
        st.lock().unwrap().phase = TunnelPhase::Busy("探测主机密钥…".to_owned());
        let observed = match handle.probe_host_key(config.clone()) {
            Ok(k) => k,
            Err(e) => {
                st.lock().unwrap().phase = TunnelPhase::Failed(format!("探测主机密钥失败: {e}"));
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
                st.lock().unwrap().phase = TunnelPhase::ConfirmHostKey {
                    fingerprint,
                    raw_key: observed.raw_key,
                    previous: None,
                };
            }
            Err(SshRuntimeError::HostKeyChanged { previous_fingerprint_sha256 }) => {
                st.lock().unwrap().phase = TunnelPhase::ConfirmHostKey {
                    fingerprint,
                    raw_key: observed.raw_key,
                    previous: Some(previous_fingerprint_sha256),
                };
            }
            Err(e) => {
                st.lock().unwrap().phase = TunnelPhase::Failed(format!("主机密钥校验失败: {e}"));
            }
        }
    });
}

/// 用户确认指纹后应用信任决策并刷新。
pub fn confirm_host_key(state: &Arc<Mutex<TunnelPaneState>>) {
    let handle = CoreHandle::new();
    let st = state.clone();
    std::thread::spawn(move || {
        let (host, port, fingerprint, raw_key, previous) = {
            let s = st.lock().unwrap();
            match &s.phase {
                TunnelPhase::ConfirmHostKey {
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
                st.lock().unwrap().phase = TunnelPhase::Failed(format!("主机密钥确认失败: {e}"));
            }
        }
    });
}

fn finish_connect(state: &Arc<Mutex<TunnelPaneState>>, fingerprint: String) {
    {
        let mut s = state.lock().unwrap();
        s.fingerprint = Some(fingerprint);
        s.phase = TunnelPhase::Ready;
    }
    refresh_profiles(state);
}

/// 刷新隧道配置列表。
pub fn refresh_profiles(state: &Arc<Mutex<TunnelPaneState>>) {
    let st = state.clone();
    std::thread::spawn(move || {
        let handle = CoreHandle::new();
        if let Ok(profiles) = handle.list_tunnel_profiles(None) {
            let ids: Vec<String> = profiles.iter().map(|p| p.id.clone()).collect();
            let mut statuses = HashMap::new();
            for id in &ids {
                if let Ok(status) = handle.poll_tunnel(id) {
                    statuses.insert(id.clone(), status.state);
                }
            }
            let mut s = st.lock().unwrap();
            s.profiles = profiles;
            s.statuses = statuses;
        }
    });
}

/// 新建隧道配置。
pub fn create_profile(state: &Arc<Mutex<TunnelPaneState>>) {
    let (session_id, kind, local_port, remote_host, remote_port) = {
        let s = state.lock().unwrap();
        (
            None,
            s.draft_kind.clone(),
            s.draft_local_port,
            s.draft_remote_host.clone(),
            s.draft_remote_port,
        )
    };
    let profile = TunnelProfile {
        id: format!(
            "tunnel-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        ),
        kind,
        local_host: "127.0.0.1".to_owned(),
        local_port,
        remote_host,
        remote_port,
    };
    if let Err(e) = CoreHandle::new().save_tunnel_profile(session_id, profile) {
        log::warn!("保存隧道失败: {e}");
    }
    refresh_profiles(state);
}

/// 启动隧道（后台，使用已确认的 SSH 上下文）。
pub fn start_profile(state: &Arc<Mutex<TunnelPaneState>>, profile_id: &str) {
    let (config, secret, fingerprint, profile) = {
        let s = state.lock().unwrap();
        let profile = s.profiles.iter().find(|p| p.id == profile_id).cloned();
        (s.config(), s.secret(), s.fingerprint.clone(), profile)
    };
    let (Some(secret), Some(fp), Some(profile)) = (secret, fingerprint, profile) else {
        return;
    };
    let st = state.clone();
    let profile_id = profile_id.to_owned();
    std::thread::spawn(move || {
        let handle = CoreHandle::new();
        match handle.start_tunnel(config, secret, &fp, profile) {
            Ok(status) => {
                st.lock().unwrap().statuses.insert(profile_id.clone(), status.state);
            }
            Err(e) => log::warn!("启动隧道失败: {e}"),
        }
    });
}

/// 关闭隧道。
pub fn stop_profile(state: &Arc<Mutex<TunnelPaneState>>, profile_id: &str) {
    let _ = CoreHandle::new().close_tunnel(profile_id);
    refresh_profiles(state);
}

/// 状态显示名。
pub fn state_label(state: &TunnelState) -> &'static str {
    match state {
        TunnelState::Stopped => "已停止",
        TunnelState::Starting => "启动中",
        TunnelState::Running => "运行中",
        TunnelState::Failed => "失败",
    }
}
