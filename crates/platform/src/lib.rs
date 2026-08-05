//! 平台适配层：平台特化能力（凭据 / 文件对话框 / 通知 / 单实例 / URL scheme）
//! 通过 trait 注入，不进共享 UI 与 Core。
//!
//! 对应 Mac 现状（见 `00-platform-architecture.md` §7）：
//! | 能力 | Mac | Windows | Linux |
//! |---|---|---|---|
//! | 凭据 | Keychain | Credential Manager | Secret Service |
//! | 文件选择 | NSOpenPanel | 原生对话框 | XDG Portal |
//! | 通知 | UserNotifications | 托盘气球 / Toast | freedesktop |
//! | 单实例 | reopen | 命名 Mutex | DBus |
//! | URL scheme | Info.plist | 注册表 | .desktop |

mod traits;

#[cfg(windows)]
mod windows;
#[cfg(not(windows))]
mod unix;

pub use traits::*;

/// 当前平台的默认实现。
pub fn default_adapter() -> Box<dyn PlatformAdapter> {
    #[cfg(windows)]
    {
        Box::new(windows::WindowsAdapter)
    }
    #[cfg(not(windows))]
    {
        Box::new(unix::UnixAdapter::new())
    }
}
