//! Unix（macOS / Linux）平台特化实现。
//!
//! PoC 阶段：单实例用 flock 真实实现；凭据 / 文件对话框 / 通知 / URL scheme
//! 标 Unsupported（正式工程接 Secret Service / XDG Portal / freedesktop 通知）。
//! macOS 上这些能力由 AppKit 原生提供，本层仅作 Win/Linux 对称占位。

use std::fs::File;
use std::io::Write;

use crate::traits::{
    CredentialStore, FileDialog, Notifier, PlatformAdapter, PlatformError, SingleInstance,
    UrlSchemeRegistrar,
};

pub struct UnixAdapter;

impl UnixAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for UnixAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl PlatformAdapter for UnixAdapter {}

impl CredentialStore for UnixAdapter {
    fn set(&self, _service: &str, _account: &str, _secret: &str) -> Result<(), PlatformError> {
        Err(PlatformError::Unsupported)
    }
    fn get(&self, _service: &str, _account: &str) -> Result<Option<String>, PlatformError> {
        Err(PlatformError::Unsupported)
    }
    fn delete(&self, _service: &str, _account: &str) -> Result<(), PlatformError> {
        Err(PlatformError::Unsupported)
    }
}

impl FileDialog for UnixAdapter {
    fn pick_file(&self, _title: &str) -> Option<String> {
        None
    }
    fn save_file(&self, _title: &str, _default_name: &str) -> Option<String> {
        None
    }
}

impl Notifier for UnixAdapter {
    fn notify(&self, _title: &str, _body: &str) -> Result<(), PlatformError> {
        Err(PlatformError::Unsupported)
    }
}

impl SingleInstance for UnixAdapter {
    fn acquire(&self) -> bool {
        // PoC：flock 锁文件。正式工程 Linux 用 DBus / Gtk.Application register。
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let path = std::env::temp_dir().join("stacio-single-instance.lock");
            let file = match File::create(&path) {
                Ok(f) => f,
                // 锁文件都建不出来：拿不到锁就不算"首实例"，fail-closed 阻止启动。
                Err(_) => return false,
            };
            let fd = file.as_raw_fd();
            // flock LOCK_EX | LOCK_NB
            let ret = libc_flock(fd, 2 | 4);
            if ret == 0 {
                // 持有 file 防释放（泄漏到进程退出即可）。
                std::mem::forget(file);
                true
            } else {
                false
            }
        }
        #[cfg(not(unix))]
        {
            true
        }
    }
}

#[cfg(unix)]
extern "C" {
    fn flock(fd: std::os::raw::c_int, operation: std::os::raw::c_int) -> std::os::raw::c_int;
}

#[cfg(unix)]
fn libc_flock(fd: std::os::raw::c_int, op: std::os::raw::c_int) -> std::os::raw::c_int {
    unsafe { flock(fd, op) }
}

impl UrlSchemeRegistrar for UnixAdapter {
    fn register(&self) -> Result<(), PlatformError> {
        // Linux：写 .desktop 到 ~/.local/share/applications 并 xdg-mime 关联。
        // PoC 仅写 .desktop 文件。
        let home = std::env::var("HOME").map_err(|e| PlatformError::Other(e.to_string()))?;
        let dir = std::path::Path::new(&home).join(".local/share/applications");
        std::fs::create_dir_all(&dir).map_err(|e| PlatformError::Other(e.to_string()))?;
        let desktop = dir.join("stacio-url-handler.desktop");
        let mut f = File::create(&desktop).map_err(|e| PlatformError::Other(e.to_string()))?;
        writeln!(
            f,
            "[Desktop Entry]\nType=Application\nName=Stacio\nExec=stacio %u\nMimeType=x-scheme-handler/stacio;\nTerminal=false"
        )
        .map_err(|e| PlatformError::Other(e.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::SingleInstance;

    /// 验证 flock 单实例：同进程内首次获取成功后，再次获取应失败。
    /// 注意 acquire() 会 forget 文件句柄（锁持有至进程退出），故本测试须独占运行。
    #[test]
    fn unix_single_instance_second_acquire_fails() {
        let adapter = UnixAdapter::new();
        let first = adapter.acquire();
        if !first {
            // 极少数情况：已有其他进程持有锁（如残留的 cargo run）。跳过断言。
            return;
        }
        let second = adapter.acquire();
        assert!(!second, "首次获取成功后，第二次获取必须失败");
    }
}
