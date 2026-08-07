//! Stacio License 授权系统。
//!
//! 对应 `docs/platform/license-integration.md`：
//! - 功能门控：8 个 entitlement（multiExec / aiAgent / bastionHost / sshTunnel /
//!   advancedMetrics / fileSync / proxyJump / sessionBulkIO）。
//! - 设备指纹：平台稳定标识 → SHA-256 哈希（macOS IOPlatformUUID / Linux machine-id /
//!   Windows MachineGuid）。
//! - 授权存储：应用数据目录 `license.json`（含签名 token + 非秘密展示字段）。
//! - 离线申请：X25519 + HKDF-SHA256 + ChaCha20-Poly1305 信封（规范 §3.1）。
//! - 授权导入：Ed25519 验签（客户端仅内置公钥，规范 §1）。
//!
//! 说明：正式后端未配置，`import` 用内置开发公钥；`dev_unlock()` 供开发/测试
//! 解锁全部功能（正式包不启用）。

use std::collections::HashSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sha2::Digest;


/// 8 个 License 门控功能标识（对应 `LicenseFeatureAccess.swift`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Feature {
    #[serde(rename = "multiExec")]
    MultiExec,
    #[serde(rename = "aiAgent")]
    AiAgent,
    #[serde(rename = "bastionHost")]
    BastionHost,
    #[serde(rename = "sshTunnel")]
    SshTunnel,
    #[serde(rename = "advancedMetrics")]
    AdvancedMetrics,
    #[serde(rename = "fileSync")]
    FileSync,
    #[serde(rename = "proxyJump")]
    ProxyJump,
    #[serde(rename = "sessionBulkIO")]
    SessionBulkIo,
}

pub const ALL_FEATURES: [Feature; 8] = [
    Feature::MultiExec,
    Feature::AiAgent,
    Feature::BastionHost,
    Feature::SshTunnel,
    Feature::AdvancedMetrics,
    Feature::FileSync,
    Feature::ProxyJump,
    Feature::SessionBulkIo,
];

impl Feature {
    pub fn label(&self) -> &'static str {
        match self {
            Feature::MultiExec => "多执行",
            Feature::AiAgent => "AI 助手",
            Feature::BastionHost => "堡垒机",
            Feature::SshTunnel => "SSH 隧道",
            Feature::AdvancedMetrics => "设备指标",
            Feature::FileSync => "文件同步",
            Feature::ProxyJump => "ProxyJump 跳板",
            Feature::SessionBulkIo => "会话批量 I/O",
        }
    }
}

/// 授权状态（稳定状态机：active/expired/suspended/revoked/invalid，规范 §2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LicenseStatus {
    Active,
    Expired,
    Suspended,
    Revoked,
    Invalid,
    Unlicensed,
}

/// 授权快照（功能模块只读此快照，不在功能里发网络校验，规范 §1.3）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LicenseSnapshot {
    pub status: LicenseStatus,
    pub product: String,
    pub username: String,
    pub email: String,
    pub plan: String,
    pub expires_at_unix: Option<i64>,
    pub entitlements: HashSet<Feature>,
    /// 完整签名 token（重导/状态同步用）。
    pub token: String,
    /// 设备指纹（防拷贝到其他设备）。
    pub device_fingerprint: String,
}

impl Default for LicenseSnapshot {
    fn default() -> Self {
        Self {
            status: LicenseStatus::Unlicensed,
            product: "stacio".to_owned(),
            username: String::new(),
            email: String::new(),
            plan: String::new(),
            expires_at_unix: None,
            entitlements: HashSet::new(),
            token: String::new(),
            device_fingerprint: String::new(),
        }
    }
}

impl LicenseSnapshot {
    pub fn is_enabled(&self, feature: Feature) -> bool {
        if self.status != LicenseStatus::Active {
            return false;
        }
        self.entitlements.contains(&feature)
    }

    pub fn is_expired(&self) -> bool {
        match self.expires_at_unix {
            Some(t) => t < 0, // 0 = 永久授权
            None => false,
        }
    }
}

// ---------------------------------------------------------------------------
// 设备指纹
// ---------------------------------------------------------------------------

/// 平台稳定标识 → SHA-256 指纹（规范 §3.1：不含路径/构建号/随机 UUID）。
pub fn device_fingerprint() -> String {
    let raw = platform_device_id();
    let digest = sha2::Sha256::digest(raw.as_bytes());
    format!("{}:{}", std::env::consts::OS, hex::encode(digest))
}

fn platform_device_id() -> String {
    #[cfg(target_os = "macos")]
    {
        // IOPlatformUUID（硬件稳定标识）。
        use std::process::Command;
        if let Ok(out) = Command::new("ioreg")
            .args(["-rd1", "-c", "IOPlatformExpertDevice"])
            .output()
        {
            let text = String::from_utf8_lossy(&out.stdout);
            for line in text.lines() {
                if let Some(idx) = line.find("IOPlatformUUID") {
                    let rest = &line[idx..];
                    if let Some(open) = rest.find('"') {
                        if let Some(close) = rest[open + 1..].find('"') {
                            return rest[open + 1..open + 1 + close].to_owned();
                        }
                    }
                }
            }
        }
        hostname_fallback()
    }
    #[cfg(target_os = "linux")]
    {
        // /etc/machine-id。
        if let Ok(id) = std::fs::read_to_string("/etc/machine-id") {
            return id.trim().to_owned();
        }
        hostname_fallback()
    }
    #[cfg(target_os = "windows")]
    {
        // MachineGuid（注册表，需读取）。
        hostname_fallback()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        hostname_fallback()
    }
}

fn hostname_fallback() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "unknown-host".to_owned())
}

// ---------------------------------------------------------------------------
// 存储
// ---------------------------------------------------------------------------

/// 授权文件路径（应用数据目录）。
pub fn license_path() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_owned());
        PathBuf::from(home).join("Library/Application Support/StacioCross/license.json")
    }
    #[cfg(target_os = "windows")]
    {
        let local = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".to_owned());
        PathBuf::from(local).join("Stacio/license.json")
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let xdg = std::env::var("XDG_DATA_HOME").unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_owned());
            format!("{home}/.local/share")
        });
        PathBuf::from(xdg).join("stacio/license.json")
    }
}

/// 授权 token 的安全存储（平台 CredentialStore，规范 §2）。
/// Linux PoC 返回 Unsupported 时回退到本地文件。
mod secure_token {
    use super::PathBuf;
    const SERVICE: &str = "Stacio";
    const ACCOUNT: &str = "license-token";

    pub fn store(token: &str) -> Result<(), String> {
        let adapter = stacio_platform::default_adapter();
        match adapter.set(SERVICE, ACCOUNT, token) {
            Ok(()) => Ok(()),
            Err(_) => {
                // 回退：本地文件（PoC）。
                std::fs::write(secure_token_path(), token).map_err(|e| e.to_string())
            }
        }
    }

    pub fn load() -> Option<String> {
        let adapter = stacio_platform::default_adapter();
        match adapter.get(SERVICE, ACCOUNT) {
            Ok(Some(t)) => Some(t),
            _ => std::fs::read_to_string(secure_token_path()).ok(),
        }
    }

    fn secure_token_path() -> PathBuf {
        super::license_path().with_file_name("license-token.bin")
    }

    #[cfg(test)]
    pub fn clear() {
        let adapter = stacio_platform::default_adapter();
        let _ = adapter.delete(SERVICE, ACCOUNT);
        let _ = std::fs::remove_file(secure_token_path());
    }
}

/// 加载授权快照；无有效签名返回 Unlicensed。
pub fn load() -> LicenseSnapshot {
    let Some(token) = secure_token::load() else {
        return LicenseSnapshot::default();
    };
    if token.is_empty() {
        return LicenseSnapshot::default();
    }
    // 重新验签（防篡改）。
    match verify_signed_token(&token) {
        Ok(parsed) => parsed,
        Err(e) => {
            log::warn!("授权文件验签失败: {e}");
            LicenseSnapshot::default()
        }
    }
}

/// 保存授权快照（token 入平台安全存储，展示字段入文件）。
pub fn save(snap: &LicenseSnapshot) -> std::io::Result<()> {
    let path = license_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut display = snap.clone();
    display.token = String::new();
    let data = serde_json::to_string_pretty(&display).expect("serialize snapshot");
    std::fs::write(path, data)?;
    if !snap.token.is_empty() {
        let _ = secure_token::store(&snap.token);
    }
    Ok(())
}

/// 开发解锁：全部功能 + 永久有效期（仅 dev 构建）。
pub fn dev_unlock() -> LicenseSnapshot {
    LicenseSnapshot {
        status: LicenseStatus::Active,
        product: "stacio".to_owned(),
        username: "dev".to_owned(),
        email: "dev@local".to_owned(),
        plan: "enterprise".to_owned(),
        expires_at_unix: Some(0), // 0 = 永久
        entitlements: ALL_FEATURES.iter().copied().collect(),
        token: String::new(),
        device_fingerprint: device_fingerprint(),
    }
}

// ---------------------------------------------------------------------------
// 离线申请信封（规范 §3.1：X25519 + HKDF-SHA256 + ChaCha20-Poly1305）
// ---------------------------------------------------------------------------

/// 生成设备申请文件（加密信封，JSON）。
pub fn generate_offline_request() -> Result<String, String> {
    use chacha20poly1305::{aead::Aead, AeadCore, ChaCha20Poly1305, KeyInit};
    use x25519_dalek::{EphemeralSecret, PublicKey};

    // 客户端内置的后台 X25519 公钥（dev key，正式包替换）。
    let server_public_bytes: [u8; 32] = [
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00,
    ];
    let server_public = PublicKey::from(server_public_bytes);

    let secret = EphemeralSecret::random_from_rng(rand::thread_rng());
    let ephemeral_public = PublicKey::from(&secret);
    let shared = secret.diffie_hellman(&server_public);

    // HKDF-SHA256 派生密钥。
    let salt = b"stacio-offline-request-v1";
    let info = b"stacio:offline-request:v1";
    let hk = hkdf::Hkdf::<sha2::Sha256>::new(Some(salt), shared.as_bytes());
    let mut key = [0u8; 32];
    hk.expand(info, &mut key).map_err(|e| e.to_string())?;

    // AEAD 加密。
    let cipher = ChaCha20Poly1305::new_from_slice(&key).map_err(|e| e.to_string())?;
    let nonce = ChaCha20Poly1305::generate_nonce(&mut rand::thread_rng());
    let payload = serde_json::json!({
        "product": "stacio",
        "deviceFingerprint": device_fingerprint(),
        "timestamp": chrono_like_now(),
    });
    let ciphertext = cipher
        .encrypt(&nonce, payload.to_string().as_bytes())
        .map_err(|e| e.to_string())?;

    let envelope = serde_json::json!({
        "protocol": "stacio-offline-request",
        "version": 1,
        "keyID": "offline-encryption-2026-01",
        "ephemeralPublicKey": base64(ephemeral_public.as_bytes()),
        "nonce": base64(&nonce),
        "ciphertext": base64(&ciphertext),
    });
    serde_json::to_string_pretty(&envelope).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// 授权导入（Ed25519 验签）
// ---------------------------------------------------------------------------

/// 授权文件载荷（导入前验签后解析）。
#[derive(Debug, Serialize, Deserialize)]
struct SignedLicense {
    product: String,
    platform: String,
    device_fingerprint: String,
    username: String,
    email: String,
    plan: String,
    expires_at_unix: i64,
    entitlements: Vec<String>,
    signature: String, // Ed25519 hex
}

/// 内置开发公钥（正式包替换为后台发布公钥，规范 §1.6 配置隔离）。
/// 对应开发私钥 seed（仅 dev/测试签名用，见 dev_sign_license）。
const DEV_PUBLIC_KEY: [u8; 32] = [
    0x23, 0x0c, 0x0e, 0xbe, 0x4a, 0x13, 0x09, 0x3b, 0x2c, 0xea, 0x91, 0xc1, 0x3f, 0x1f, 0x8e, 0xe1,
    0x08, 0x36, 0xd1, 0xc3, 0xbb, 0x4d, 0x6e, 0xa1, 0x9f, 0x1b, 0x6e, 0x28, 0x9c, 0x3e, 0x64, 0x5a,
];

/// 开发私钥 seed（仅用于生成测试授权文件，正式包不含）。
const DEV_PRIVATE_SEED: [u8; 32] = [
    0x5a, 0x54, 0x41, 0x43, 0x49, 0x4f, 0x44, 0x45, 0x56, 0x4b, 0x45, 0x59, 0x30, 0x31, 0x32, 0x33,
    0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x31, 0x32, 0x33, 0x34,
];

/// 生成开发用授权文件（仅测试/调试；正式由后台签发）。
/// features 用 Feature 标签逗号分隔，如 "sshTunnel,multiExec"；expires_at_unix=0 为永久。
pub fn dev_sign_license(path: &str, username: &str, email: &str, plan: &str, features: &str) -> Result<(), String> {
    use ed25519_dalek::Signer;
    let sk = ed25519_dalek::SigningKey::from_bytes(&DEV_PRIVATE_SEED);
    let entitlements: Vec<String> = features
        .split(',')
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect();
    let license = SignedLicense {
        product: "stacio".to_owned(),
        platform: std::env::consts::OS.to_owned(),
        device_fingerprint: device_fingerprint(),
        username: username.to_owned(),
        email: email.to_owned(),
        plan: plan.to_owned(),
        expires_at_unix: 0,
        entitlements,
        signature: String::new(),
    };
    let signing_text = canonical_license_text(&license);
    let sig = sk.sign(signing_text.as_bytes());
    let mut license = license;
    license.signature = hex::encode(sig.to_bytes());
    let data = serde_json::to_string_pretty(&license).map_err(|e| e.to_string())?;
    std::fs::write(path, data).map_err(|e| e.to_string())?;
    Ok(())
}

/// 导入 `.stacio-license` 文件：验签 → 验产品/平台/设备 → 存快照。
pub fn import_license_file(path: &str) -> Result<LicenseSnapshot, String> {
    let data = std::fs::read_to_string(path).map_err(|e| format!("读取授权文件失败: {e}"))?;
    let license: SignedLicense =
        serde_json::from_str(&data).map_err(|e| format!("授权文件格式错误: {e}"))?;

    // 验签：对非签名字段做规范化 JSON 再签名。
    let signing_text = canonical_license_text(&license);
    let public = ed25519_dalek::VerifyingKey::from_bytes(&DEV_PUBLIC_KEY)
        .map_err(|e| format!("公钥无效: {e}"))?;
    let sig_bytes = hex::decode(&license.signature).map_err(|e| format!("签名格式错误: {e}"))?;
    let sig = ed25519_dalek::Signature::from_slice(&sig_bytes)
        .map_err(|e| format!("签名格式错误: {e}"))?;
    public
        .verify_strict(signing_text.as_bytes(), &sig)
        .map_err(|_| "授权签名无效".to_string())?;

    // 验产品 / 平台 / 设备。
    if license.product != "stacio" {
        return Err("产品不匹配".to_string());
    }
    if license.platform != std::env::consts::OS {
        return Err("平台不匹配".to_string());
    }
    if license.device_fingerprint != device_fingerprint() {
        return Err("设备不匹配".to_string());
    }

    let mut entitlements = HashSet::new();
    for name in &license.entitlements {
        if let Some(f) = ALL_FEATURES.iter().find(|f| feature_name(**f) == *name) {
            entitlements.insert(*f);
        }
    }

    let snap = LicenseSnapshot {
        status: if license.expires_at_unix < 0 {
            LicenseStatus::Expired
        } else {
            LicenseStatus::Active
        },
        product: license.product.clone(),
        username: license.username.clone(),
        email: license.email.clone(),
        plan: license.plan.clone(),
        expires_at_unix: Some(license.expires_at_unix),
        entitlements,
        token: data,
        device_fingerprint: license.device_fingerprint.clone(),
    };
    save(&snap).map_err(|e| format!("保存授权失败: {e}"))?;
    Ok(snap)
}

/// 验证已存 token（重新加载时调用）。
fn verify_signed_token(token: &str) -> Result<LicenseSnapshot, String> {
    let license: SignedLicense =
        serde_json::from_str(token).map_err(|e| format!("授权格式错误: {e}"))?;
    let signing_text = canonical_license_text(&license);
    let public = ed25519_dalek::VerifyingKey::from_bytes(&DEV_PUBLIC_KEY)
        .map_err(|e| format!("公钥无效: {e}"))?;
    let sig_bytes = hex::decode(&license.signature).map_err(|e| format!("签名格式错误: {e}"))?;
    let sig = ed25519_dalek::Signature::from_slice(&sig_bytes)
        .map_err(|e| format!("签名格式错误: {e}"))?;
    public
        .verify_strict(signing_text.as_bytes(), &sig)
        .map_err(|_| "授权签名无效".to_string())?;

    let mut entitlements = HashSet::new();
    for name in &license.entitlements {
        if let Some(f) = ALL_FEATURES.iter().find(|f| feature_name(**f) == *name) {
            entitlements.insert(*f);
        }
    }
    Ok(LicenseSnapshot {
        status: if license.expires_at_unix < 0 {
            LicenseStatus::Expired
        } else {
            LicenseStatus::Active
        },
        product: license.product,
        username: license.username,
        email: license.email,
        plan: license.plan,
        expires_at_unix: Some(license.expires_at_unix),
        entitlements,
        token: token.to_owned(),
        device_fingerprint: license.device_fingerprint,
    })
}

/// 规范化签名文本（签名字段排除，其余字段按固定顺序）。
fn canonical_license_text(l: &SignedLicense) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}|{}|{}",
        l.product,
        l.platform,
        l.device_fingerprint,
        l.username,
        l.email,
        l.plan,
        l.expires_at_unix,
        l.entitlements.join(",")
    )
}

fn feature_name(f: Feature) -> &'static str {
    match f {
        Feature::MultiExec => "multiExec",
        Feature::AiAgent => "aiAgent",
        Feature::BastionHost => "bastionHost",
        Feature::SshTunnel => "sshTunnel",
        Feature::AdvancedMetrics => "advancedMetrics",
        Feature::FileSync => "fileSync",
        Feature::ProxyJump => "proxyJump",
        Feature::SessionBulkIo => "sessionBulkIO",
    }
}

// ---------------------------------------------------------------------------
// 工具
// ---------------------------------------------------------------------------

fn base64(data: &[u8]) -> String {
    // RFC 4648 Base64 编码（规范 §3.1 要求 Base64；无外部依赖的轻量实现）。
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(TABLE[(n >> 6) as usize & 63] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[n as usize & 63] as char);
        } else {
            out.push('=');
        }
    }
    out
}

fn chrono_like_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_fingerprint_is_stable_and_scoped() {
        let a = device_fingerprint();
        let b = device_fingerprint();
        assert_eq!(a, b, "同设备指纹应稳定");
        assert!(a.starts_with(&format!("{}:", std::env::consts::OS)), "指纹必须含平台域");
    }

    #[test]
    fn sign_import_verify_roundtrip() {
        // 生成测试授权 → 导入（验签+验设备）→ 门控检查。
        let dir = std::env::temp_dir().join(format!("stacio-license-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("dev.stacio-license");
        dev_sign_license(
            file.to_str().unwrap(),
            "tester",
            "t@example.com",
            "pro",
            "sshTunnel,multiExec",
        )
        .expect("sign");

        let snap = import_license_file(file.to_str().unwrap()).expect("import");
        assert_eq!(snap.status, LicenseStatus::Active);
        assert!(snap.is_enabled(Feature::SshTunnel));
        assert!(snap.is_enabled(Feature::MultiExec));
        assert!(!snap.is_enabled(Feature::AiAgent), "未授权的功能必须为 false");

        // 重新 load 也应验签通过（模拟重启）。
        let reloaded = load();
        assert_eq!(reloaded.status, LicenseStatus::Active);

        // 清理（避免污染后续测试）。
        std::fs::remove_file(license_path()).ok();
        secure_token::clear();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dev_unlock_enables_everything() {
        let snap = dev_unlock();
        for f in ALL_FEATURES {
            assert!(snap.is_enabled(f));
        }
    }

    #[test]
    fn tampered_token_is_rejected() {
        let mut snap = dev_unlock();
        // 篡改后 load 应回到 Unlicensed。
        snap.token = "tampered".to_owned();
        save(&snap).unwrap();
        let loaded = load();
        assert_eq!(loaded.status, LicenseStatus::Unlicensed);
        std::fs::remove_file(license_path()).ok();
        secure_token::clear();
    }

    #[test]
    fn base64_rfc4648_vectors() {
        // RFC 4648 §10 测试向量。
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }
}

