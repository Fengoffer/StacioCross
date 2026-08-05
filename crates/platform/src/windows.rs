//! Windows 平台特化实现。
//!
//! 凭据 → Credential Manager；文件选择 → 原生 GetOpen/SaveFileName；
//! 通知 → 托盘气球（Shell_NotifyIcon）；单实例 → 命名 Mutex；
//! URL scheme → 注册表 HKEY_CLASSES_ROOT。
//!
//! 本机（macOS）仅做 `cargo check --target x86_64-pc-windows-msvc` 编译验证；
//! 运行级验证需 Windows 测试机 / CI runner。
//!
//! 注意：windows-sys 0.60 中 `PWSTR`/`PCWSTR` 是裸指针类型别名
//! （`*mut u16` / `*const u16`），不是 newtype，不能当构造函数用。

use crate::traits::{
    CredentialStore, FileDialog, Notifier, PlatformAdapter, PlatformError, SingleInstance,
    UrlSchemeRegistrar,
};

/// 字符串 → UTF-16（含 null 终止）。
fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub struct WindowsAdapter;

impl PlatformAdapter for WindowsAdapter {}

// ---------------------------------------------------------------------------
// 凭据：Windows Credential Manager
// ---------------------------------------------------------------------------

impl CredentialStore for WindowsAdapter {
    fn set(&self, service: &str, account: &str, secret: &str) -> Result<(), PlatformError> {
        use windows_sys::Win32::Security::Credentials::*;
        unsafe {
            let target = to_wide(&format!("{service}/{account}"));
            let user = to_wide(account);
            let mut blob: Vec<u8> = secret.as_bytes().to_vec();
            let cred = CREDENTIALW {
                Flags: 0,
                Type: CRED_TYPE_GENERIC,
                TargetName: target.as_ptr() as *mut u16,
                Comment: std::ptr::null_mut(),
                LastWritten: std::mem::zeroed(),
                CredentialBlobSize: blob.len() as u32,
                CredentialBlob: blob.as_mut_ptr(),
                Persist: CRED_PERSIST_LOCAL_MACHINE,
                AttributeCount: 0,
                Attributes: std::ptr::null_mut(),
                TargetAlias: std::ptr::null_mut(),
                UserName: user.as_ptr() as *mut u16,
            };
            if CredWriteW(&cred, 0) == 0 {
                return Err(PlatformError::Api(last_error()));
            }
            Ok(())
        }
    }

    fn get(&self, service: &str, account: &str) -> Result<Option<String>, PlatformError> {
        use windows_sys::Win32::Security::Credentials::*;
        unsafe {
            let target = to_wide(&format!("{service}/{account}"));
            let mut pcred: *mut CREDENTIALW = std::ptr::null_mut();
            if CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut pcred) == 0 {
                let err = last_error();
                // ERROR_NOT_FOUND = 1168
                if err == 1168 {
                    return Ok(None);
                }
                return Err(PlatformError::Api(err));
            }
            let cred = &*pcred;
            let bytes =
                std::slice::from_raw_parts(cred.CredentialBlob, cred.CredentialBlobSize as usize);
            let value = String::from_utf8_lossy(bytes).into_owned();
            CredFree(pcred as *const _);
            Ok(Some(value))
        }
    }

    fn delete(&self, service: &str, account: &str) -> Result<(), PlatformError> {
        use windows_sys::Win32::Security::Credentials::*;
        unsafe {
            let target = to_wide(&format!("{service}/{account}"));
            if CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) == 0 {
                let err = last_error();
                if err == 1168 {
                    return Ok(());
                }
                return Err(PlatformError::Api(err));
            }
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// 文件选择：原生对话框
// ---------------------------------------------------------------------------

impl FileDialog for WindowsAdapter {
    fn pick_file(&self, title: &str) -> Option<String> {
        self.open_save_dialog(title, "", false)
    }

    fn save_file(&self, title: &str, default_name: &str) -> Option<String> {
        self.open_save_dialog(title, default_name, true)
    }
}

impl WindowsAdapter {
    fn open_save_dialog(&self, title: &str, default_name: &str, save: bool) -> Option<String> {
        use windows_sys::Win32::UI::Controls::Dialogs::*;
        unsafe {
            let mut file_buf = vec![0u16; 1024];
            let default_wide = to_wide(default_name);
            for (i, c) in default_wide.iter().enumerate() {
                if i < file_buf.len() {
                    file_buf[i] = *c;
                }
            }
            let title_wide = to_wide(title);
            let filter = to_wide("All Files\0*.*\0");
            let mut ofn: OPENFILENAMEW = std::mem::zeroed();
            ofn.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
            ofn.lpstrFile = file_buf.as_mut_ptr();
            ofn.nMaxFile = file_buf.len() as u32;
            ofn.lpstrFilter = filter.as_ptr();
            ofn.lpstrTitle = title_wide.as_ptr();
            ofn.Flags = OFN_EXPLORER | OFN_PATHMUSTEXIST | OFN_FILEMUSTEXIST;

            let ok = if save {
                GetSaveFileNameW(&mut ofn)
            } else {
                GetOpenFileNameW(&mut ofn)
            };
            if ok == 0 {
                return None;
            }
            let len = file_buf.iter().position(|&c| c == 0).unwrap_or(file_buf.len());
            Some(String::from_utf16_lossy(&file_buf[..len]))
        }
    }
}

// ---------------------------------------------------------------------------
// 通知：托盘气球
// ---------------------------------------------------------------------------

impl Notifier for WindowsAdapter {
    fn notify(&self, title: &str, body: &str) -> Result<(), PlatformError> {
        use windows_sys::Win32::UI::Shell::*;
        unsafe {
            let mut data: NOTIFYICONDATAW = std::mem::zeroed();
            data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
            data.uFlags = NIF_INFO;
            copy_into(&mut data.szInfoTitle, title);
            copy_into(&mut data.szInfo, body);
            data.dwInfoFlags = NIIF_INFO;
            if Shell_NotifyIconW(NIM_ADD, &data) == 0 {
                return Err(PlatformError::Api(last_error()));
            }
            Ok(())
        }
    }
}

/// 把 &str 拷入固定长度 UTF-16 数组。
fn copy_into(dst: &mut [u16], src: &str) {
    for (i, c) in src.encode_utf16().enumerate() {
        if i >= dst.len() - 1 {
            break;
        }
        dst[i] = c;
    }
}

// ---------------------------------------------------------------------------
// 单实例：命名 Mutex
// ---------------------------------------------------------------------------

impl SingleInstance for WindowsAdapter {
    fn acquire(&self) -> bool {
        use windows_sys::Win32::System::Threading::*;
        unsafe {
            let name = to_wide("Local\\StacioSingleInstance");
            let handle = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
            if handle.is_null() {
                // Mutex 创建失败：拿不到锁就不算"首实例"，fail-closed 阻止启动。
                return false;
            }
            // ERROR_ALREADY_EXISTS = 183：已有实例持有该命名 Mutex。
            last_error() != 183
        }
    }
}

// ---------------------------------------------------------------------------
// URL scheme：注册表
// ---------------------------------------------------------------------------

impl UrlSchemeRegistrar for WindowsAdapter {
    fn register(&self) -> Result<(), PlatformError> {
        use windows_sys::Win32::System::Registry::*;
        unsafe {
            // HKEY_CLASSES_ROOT\stacio 下写 "URL Protocol"=""，标记为协议处理器。
            let key = to_wide("stacio");
            let url_proto = to_wide("URL Protocol");
            let empty = to_wide("");
            let status = RegSetKeyValueW(
                HKEY_CLASSES_ROOT,
                key.as_ptr(),
                url_proto.as_ptr(),
                REG_SZ,
                empty.as_ptr() as *const std::ffi::c_void,
                (empty.len() * 2) as u32,
            );
            if status != 0 {
                return Err(PlatformError::Api(status as u32));
            }
            Ok(())
        }
    }
}

fn last_error() -> u32 {
    unsafe { windows_sys::Win32::Foundation::GetLastError() }
}

#[cfg(test)]
mod tests {
    use crate::traits::{CredentialStore, SingleInstance};

    const TEST_SERVICE: &str = "stacio-platform-test";
    const TEST_ACCOUNT: &str = "roundtrip";

    /// 真实调用 Credential Manager：写入 → 读取校验 → 删除 → 确认读不到。
    #[test]
    fn windows_credential_roundtrip() {
        let adapter = super::WindowsAdapter;
        // 清理可能残留的测试凭据。
        let _ = adapter.delete(TEST_SERVICE, TEST_ACCOUNT);

        adapter
            .set(TEST_SERVICE, TEST_ACCOUNT, "s3cr3t")
            .expect("CredWriteW");

        let got = adapter.get(TEST_SERVICE, TEST_ACCOUNT).expect("CredReadW");
        assert_eq!(got.as_deref(), Some("s3cr3t"));

        adapter
            .delete(TEST_SERVICE, TEST_ACCOUNT)
            .expect("CredDeleteW");

        let after = adapter
            .get(TEST_SERVICE, TEST_ACCOUNT)
            .expect("CredReadW after delete");
        assert_eq!(after, None);
    }

    /// 真实调用 CreateMutexW：首次获取成功后，第二次应观察到 ERROR_ALREADY_EXISTS。
    #[test]
    fn windows_single_instance_second_acquire_fails() {
        let adapter = super::WindowsAdapter;
        let first = adapter.acquire();
        if !first {
            return; // 极少数情况：已有实例持有 Mutex。
        }
        let second = adapter.acquire();
        assert!(!second, "首次获取成功后，第二次获取必须失败");
    }
}
