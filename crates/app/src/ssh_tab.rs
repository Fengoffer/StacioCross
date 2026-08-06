//! 会话标签：连接状态机 + 输出泵 + 终端键位映射。
//!
//! 支持 SSH / Telnet / 串口（功能清单 2.1 / 6.18 / 6.19）。
//!
//! 流程（对应 stacio_core live shell API）：
//! - SSH：Auth → probe_host_key → 决策探针（Reject）→
//!   [已知且匹配 → 直接连] / [未知 → 用户确认指纹] / [变更 → 用户确认替换] →
//!   start_live_ssh_shell_runtime → Running{runtime_id} → 输出泵喂 TerminalModel。
//! - Telnet / Serial：无需探测与认证 → 直接 start_live_{telnet,serial}_shell_runtime。
//!
//! SSH 认证：密码或 SSH Agent（私钥留后续里程碑）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use stacio_core_bridge::{
    CoreHandle, HostKeyTrustDecision, LiveShellStatus, SerialConnectionConfig, SshAuthMethod,
    SshAuthSecret, SshConnectionConfig, SshRuntimeError, TelnetConnectionConfig,
};
use stacio_term::model::TerminalModel;

/// 会话协议类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    Ssh,
    Telnet,
    Serial,
}

/// SSH 标签的阶段。
#[derive(Debug, Clone)]
pub enum SshPhase {
    /// 等待用户填写认证信息。
    Auth,
    /// 探测 / 连接中（携带提示文案）。
    Busy(String),
    /// 等待用户确认主机指纹。previous = Some(旧指纹) 表示"密钥已变更"。
    ConfirmHostKey {
        fingerprint: String,
        raw_key: Vec<u8>,
        previous: Option<String>,
    },
    /// 已连接，运行中。
    Running { runtime_id: String },
    /// 失败（携带诊断信息）。
    Failed { message: String },
    /// 已关闭。
    Closed,
}

/// 会话标签状态（供工作台 UI 渲染与事件驱动）。
pub struct SshTabState {
    /// 协议类型。
    pub kind: ShellKind,
    /// SSH/Telnet 的主机；Serial 的设备路径。
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub use_agent: bool,
    /// Serial 波特率。
    pub baud_rate: u32,
    pub phase: SshPhase,
    /// 待确认的多行粘贴内容（功能清单 2.18）。
    pub pending_paste: Option<String>,
    /// SSH keepalive 间隔（秒，0=关闭，功能清单 2.15）。
    pub keepalive_seconds: u32,
    /// 命令历史（功能清单 2.20）：按用户输入的行记录（含时间近似顺序）。
    pub command_history: Vec<String>,
    /// 当前行缓冲（命令历史跟踪用）。
    pub current_line: String,
    /// 宏录制状态（功能清单 2.21）。
    pub recording: bool,
    /// 录制中的步骤。
    pub record_steps: Vec<stacio_core_bridge::MacroStep>,
    /// 上一步时间（计算 delay_ms）。
    last_step_at: std::time::Instant,
    /// 已上报 core 的尺寸（避免每帧重复 record_resize）。
    pub last_report_cols: u32,
    pub last_report_rows: u32,
    /// 输出泵停止信号。
    pub poll_stop: Arc<AtomicBool>,
}

impl SshTabState {
    pub fn new(host: &str, port: u16, username: &str) -> Self {
        Self {
            kind: ShellKind::Ssh,
            host: host.to_owned(),
            port,
            username: username.to_owned(),
            password: String::new(),
            use_agent: false,
            baud_rate: 115_200,
            phase: SshPhase::Auth,
            pending_paste: None,
            keepalive_seconds: 30,
            command_history: Vec::new(),
            current_line: String::new(),
            recording: false,
            record_steps: Vec::new(),
            last_step_at: std::time::Instant::now(),
            last_report_cols: 0,
            last_report_rows: 0,
            poll_stop: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn new_telnet(host: &str, port: u16) -> Self {
        let mut s = Self::new(host, port, "");
        s.kind = ShellKind::Telnet;
        s
    }

    pub fn new_serial(device_path: &str, baud_rate: u32) -> Self {
        let mut s = Self::new(device_path, 0, "");
        s.kind = ShellKind::Serial;
        s.baud_rate = baud_rate;
        s
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

    fn set_phase(&mut self, phase: SshPhase) {
        self.phase = phase;
    }

    /// 跟踪输入行 → 命令历史（功能清单 2.20）；录制时记步骤（功能清单 2.21）。
    pub fn feed_input(&mut self, bytes: &[u8]) {
        if self.recording && !bytes.is_empty() {
            let now = std::time::Instant::now();
            let delay_ms = now.duration_since(self.last_step_at).as_millis().min(u32::MAX as u128) as u32;
            let input = String::from_utf8_lossy(bytes).into_owned();
            self.record_steps.push(stacio_core_bridge::MacroStep {
                order: self.record_steps.len() as u32,
                input,
                delay_ms,
            });
            self.last_step_at = now;
        }
        for &b in bytes {
            match b {
                b'\r' | b'\n' => {
                    let cmd = self.current_line.trim().to_string();
                    if !cmd.is_empty() {
                        self.command_history.push(cmd);
                        if self.command_history.len() > 200 {
                            self.command_history.remove(0);
                        }
                    }
                    self.current_line.clear();
                }
                0x08 | 0x7f => {
                    self.current_line.pop();
                }
                b if b.is_ascii_control() => {}
                _ => self.current_line.push(b as char),
            }
        }
    }

    /// 开始 / 结束录制。返回录制的步骤（结束时）。
    pub fn toggle_recording(&mut self) -> Vec<stacio_core_bridge::MacroStep> {
        if self.recording {
            self.recording = false;
            let steps = std::mem::take(&mut self.record_steps);
            self.last_step_at = std::time::Instant::now();
            steps
        } else {
            self.recording = true;
            self.record_steps.clear();
            self.last_step_at = std::time::Instant::now();
            Vec::new()
        }
    }
}

/// 在后台线程开始连接。
/// SSH：探测主机密钥 + 决策探针（Reject），分派到 ConfirmHostKey / 直接连接 / Failed。
/// Telnet / Serial：无需探测与认证，直接连接。
pub fn begin_connect(state: &Arc<Mutex<SshTabState>>, model: Arc<Mutex<TerminalModel>>) {
    let handle = CoreHandle::new();
    let st = state.clone();
    std::thread::spawn(move || {
        // Telnet / Serial：直接连接。
        if st.lock().unwrap().kind != ShellKind::Ssh {
            do_connect(&st, model, None);
            return;
        }

        let config = {
            let s = st.lock().unwrap();
            s.config()
        };

        // 1) 探测主机密钥（连接即断）。
        let observed = match handle.probe_host_key(config.clone()) {
            Ok(key) => key,
            Err(e) => {
                st.lock().unwrap().set_phase(SshPhase::Failed {
                    message: format!("探测主机密钥失败: {e}"),
                });
                return;
            }
        };
        let fingerprint = observed.fingerprint_sha256.clone();

        // 2) 决策探针：Reject 用作"查询"，区分 已知匹配 / 未知 / 已变更。
        match handle.apply_host_key_decision(
            &config.host,
            config.port,
            observed.raw_key.clone(),
            HostKeyTrustDecision::Reject,
        ) {
            Ok(_) => {
                // 已知且匹配 → 直接连接。
                do_connect(&st, model, Some(fingerprint));
            }
            Err(SshRuntimeError::UnknownHostKey) => {
                st.lock().unwrap().set_phase(SshPhase::ConfirmHostKey {
                    fingerprint,
                    raw_key: observed.raw_key,
                    previous: None,
                });
            }
            Err(SshRuntimeError::HostKeyChanged { previous_fingerprint_sha256 }) => {
                st.lock().unwrap().set_phase(SshPhase::ConfirmHostKey {
                    fingerprint,
                    raw_key: observed.raw_key,
                    previous: Some(previous_fingerprint_sha256),
                });
            }
            Err(e) => {
                st.lock().unwrap().set_phase(SshPhase::Failed {
                    message: format!("主机密钥校验失败: {e}"),
                });
            }
        }
    });
}

/// 用户确认指纹后调用（仅 SSH）：应用信任决策（TrustAndSave / TrustAndReplace）并连接。
pub fn confirm_host_key(
    state: &Arc<Mutex<SshTabState>>,
    model: Arc<Mutex<TerminalModel>>,
) {
    let handle = CoreHandle::new();
    let st = state.clone();
    std::thread::spawn(move || {
        let (host, port, fingerprint, raw_key, previous) = {
            let s = st.lock().unwrap();
            match &s.phase {
                SshPhase::ConfirmHostKey {
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
            Ok(_) => do_connect(&st, model, Some(fingerprint)),
            Err(e) => {
                st.lock().unwrap().set_phase(SshPhase::Failed {
                    message: format!("主机密钥确认失败: {e}"),
                });
            }
        }
    });
}

/// 启动 live shell（按协议分派），成功后转 Running 并启动输出泵。
fn do_connect(
    state: &Arc<Mutex<SshTabState>>,
    model: Arc<Mutex<TerminalModel>>,
    expected_fingerprint: Option<String>,
) {
    let handle = CoreHandle::new();
    let st = state.clone();
    let (cols, rows) = {
        let m = model.lock().unwrap();
        let sz = m.size();
        (sz.columns as u32, sz.rows as u32)
    };

    st.lock().unwrap().set_phase(SshPhase::Busy("连接中…".to_owned()));

    let outcome = {
        let s = st.lock().unwrap();
        match s.kind {
            ShellKind::Ssh => {
                let Some(secret) = s.secret() else {
                    drop(s);
                    st.lock().unwrap().set_phase(SshPhase::Failed {
                        message: "未提供认证信息（密码为空且未选 Agent）".to_owned(),
                    });
                    return;
                };
                handle.start_ssh_shell(
                    s.config(),
                    secret,
                    expected_fingerprint.unwrap_or_default(),
                    cols,
                    rows,
                )
            }
            ShellKind::Telnet => handle.start_telnet_shell(
                TelnetConnectionConfig {
                    host: s.host.clone(),
                    port: s.port,
                    username: None,
                    connect_timeout_ms: 10_000,
                },
                cols,
                rows,
            ),
            ShellKind::Serial => handle.start_serial_shell(
                SerialConnectionConfig {
                    device_path: s.host.clone(),
                    baud_rate: s.baud_rate,
                    data_bits: 8,
                    stop_bits: 1,
                    parity: "none".to_owned(),
                    flow_control: "none".to_owned(),
                    backspace_mode: "ctrl_h".to_owned(),
                },
                cols,
                rows,
            ),
        }
    };

    match outcome {
        Ok(LiveShellStatus { runtime_id, status, .. }) if status == "running" => {
            let keepalive = {
                let mut s = st.lock().unwrap();
                s.phase = SshPhase::Running {
                    runtime_id: runtime_id.clone(),
                };
                s.last_report_cols = cols;
                s.last_report_rows = rows;
                s.poll_stop = Arc::new(AtomicBool::new(false));
                s.keepalive_seconds
            };
            // 应用 SSH keepalive 间隔（功能清单 2.15）。
            if keepalive > 0 {
                let _ = handle.set_keepalive_interval(&runtime_id, keepalive);
            }
            spawn_output_pump(&st, model, runtime_id);
        }
        Ok(status) => {
            st.lock().unwrap().set_phase(SshPhase::Failed {
                message: format!("连接失败: {} ({})", status.diagnostic, status.status),
            });
        }
        Err(e) => {
            st.lock().unwrap().set_phase(SshPhase::Failed {
                message: format!("连接失败: {e}"),
            });
        }
    }
}

/// 输出泵：每 50ms 取输出批次喂 TerminalModel，并轮询状态。
fn spawn_output_pump(
    state: &Arc<Mutex<SshTabState>>,
    model: Arc<Mutex<TerminalModel>>,
    runtime_id: String,
) {
    let handle = CoreHandle::new();
    let st = state.clone();
    let stop = {
        let s = st.lock().unwrap();
        s.poll_stop.clone()
    };
    std::thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            match handle.take_output(&runtime_id) {
                Ok(batch) => {
                    if !batch.bytes.is_empty() {
                        if let Ok(mut m) = model.lock() {
                            m.process_bytes(&batch.bytes);
                        }
                    }
                }
                Err(_) => break, // runtime 已关闭
            }
            match handle.poll_ssh_shell(&runtime_id) {
                Ok(s) if s.status == "failed" => {
                    st.lock().unwrap().set_phase(SshPhase::Failed {
                        message: s.diagnostic,
                    });
                    break;
                }
                Ok(s) if s.status == "closed" => {
                    st.lock().unwrap().set_phase(SshPhase::Closed);
                    break;
                }
                Ok(_) => {}
                Err(_) => break,
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    });
}

/// 关闭 SSH 运行时（标签关闭时调用）。
pub fn close_runtime(state: &Arc<Mutex<SshTabState>>) {
    let (stop, runtime_id) = {
        let s = state.lock().unwrap();
        (
            s.poll_stop.clone(),
            match &s.phase {
                SshPhase::Running { runtime_id } => Some(runtime_id.clone()),
                _ => None,
            },
        )
    };
    stop.store(true, Ordering::Relaxed);
    if let Some(rid) = runtime_id {
        let _ = CoreHandle::new().close_runtime(&rid);
    }
}

/// 报告尺寸变更（仅在变化时调 core 的 record_resize）。
pub fn report_resize(state: &Arc<Mutex<SshTabState>>, cols: u32, rows: u32) {
    let mut s = state.lock().unwrap();
    if s.last_report_cols == cols && s.last_report_rows == rows {
        return;
    }
    if let SshPhase::Running { runtime_id } = &s.phase {
        let rid = runtime_id.clone();
        s.last_report_cols = cols;
        s.last_report_rows = rows;
        drop(s);
        let _ = CoreHandle::new().record_resize(&rid, cols, rows);
    } else {
        s.last_report_cols = cols;
        s.last_report_rows = rows;
    }
}

/// egui 按键 → 终端字节序列（基础键位映射）。
pub fn terminal_key_bytes(key: egui::Key, modifiers: egui::Modifiers) -> Option<Vec<u8>> {
    use egui::Key::*;
    let base = match key {
        Enter => Some(b"\r".to_vec()),
        Backspace => Some(b"\x7f".to_vec()),
        Tab => Some(b"\t".to_vec()),
        Escape => Some(b"\x1b".to_vec()),
        ArrowUp => Some(b"\x1b[A".to_vec()),
        ArrowDown => Some(b"\x1b[B".to_vec()),
        ArrowRight => Some(b"\x1b[C".to_vec()),
        ArrowLeft => Some(b"\x1b[D".to_vec()),
        Home => Some(b"\x1b[H".to_vec()),
        End => Some(b"\x1b[F".to_vec()),
        PageUp => Some(b"\x1b[5~".to_vec()),
        PageDown => Some(b"\x1b[6~".to_vec()),
        Delete => Some(b"\x1b[3~".to_vec()),
        _ => None,
    };
    if let Some(bytes) = base {
        return Some(bytes);
    }
    // 字母键（egui::Key 的 A..=Z 判别值连续）：Ctrl+A..Z → 0x01..0x1a；Alt+letter → ESC+letter。
    let key_a = egui::Key::A as u8;
    let key_z = egui::Key::Z as u8;
    let idx = key as u8;
    if idx >= key_a && idx <= key_z {
        let letter = (b'a' + (idx - key_a)) as char;
        if modifiers.ctrl {
            return Some(vec![idx - key_a + 1]);
        }
        if modifiers.alt {
            return Some(format!("\x1b{letter}").into_bytes());
        }
    }
    None
}
