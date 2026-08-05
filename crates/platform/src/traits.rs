//! 平台适配 trait。共享 UI / Core 只依赖这些接口，不感知具体平台。

/// 凭据存储（App 级密钥，如 License vault key）。业务凭据不进此层。
pub trait CredentialStore {
    /// 写入一条凭据。
    fn set(&self, service: &str, account: &str, secret: &str) -> Result<(), PlatformError>;
    /// 读取一条凭据。
    fn get(&self, service: &str, account: &str) -> Result<Option<String>, PlatformError>;
    /// 删除一条凭据。
    fn delete(&self, service: &str, account: &str) -> Result<(), PlatformError>;
}

/// 原生文件选择对话框。
pub trait FileDialog {
    /// 打开"选择文件"对话框，返回选中路径（取消 = None）。
    fn pick_file(&self, title: &str) -> Option<String>;
    /// 打开"保存文件"对话框，返回目标路径（取消 = None）。
    fn save_file(&self, title: &str, default_name: &str) -> Option<String>;
}

/// 系统通知。
pub trait Notifier {
    /// 弹出一条通知（标题 + 正文）。
    fn notify(&self, title: &str, body: &str) -> Result<(), PlatformError>;
}

/// 单实例锁：防止多开，第二实例应把参数交给第一实例。
pub trait SingleInstance {
    /// 尝试获取单实例锁。已运行返回 false。
    fn acquire(&self) -> bool;
}

/// URL scheme 注册（`stacio://`）。
pub trait UrlSchemeRegistrar {
    /// 注册本应用为 `stacio://` 的处理器。
    fn register(&self) -> Result<(), PlatformError>;
}

/// 平台错误。
#[derive(Debug)]
pub enum PlatformError {
    /// 平台 API 调用失败（携带平台错误码）。
    Api(u32),
    /// 不支持。
    Unsupported,
    /// 其他。
    Other(String),
}

impl std::fmt::Display for PlatformError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlatformError::Api(code) => write!(f, "platform api error: {code}"),
            PlatformError::Unsupported => write!(f, "unsupported on this platform"),
            PlatformError::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for PlatformError {}

/// 聚合接口：一个平台实现全部特化能力。
pub trait PlatformAdapter: CredentialStore + FileDialog + Notifier + SingleInstance + UrlSchemeRegistrar {}
