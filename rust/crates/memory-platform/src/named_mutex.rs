//! Windows 命名互斥锁：跨进程同步原语。
//!
//! 用途：数据库结构迁移锁 `Local\MemStack.Database.Migration`（迁移执行计划 §6.3）。
//! 语义：`CreateMutexW` 创建或打开；`WaitForSingleObject` 等待所有权；
//! 原持有者异常退出时等待方收到 `Abandoned` 并获得所有权（可安全接管，迁移操作均为短事务）。

use memory_domain::{BusinessError, ErrorCode};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};
use windows::core::PCWSTR;

/// 等待结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitResult {
    /// 成功取得所有权。
    Acquired,
    /// 超时未取得。
    Timeout,
    /// 原持有者异常退出，所有权转移给当前进程（等价于取得）。
    Abandoned,
}

/// Windows 命名互斥锁（RAII：Drop 关闭句柄；所有权需显式 `release`）。
pub struct NamedMutex {
    handle: HANDLE,
}

impl NamedMutex {
    /// 创建或打开命名互斥锁（初始不持有所有权）。
    pub fn create(name: &str) -> Result<Self, BusinessError> {
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: wide 以 NUL 结尾；句柄由 Self 管理。
        let handle = unsafe { CreateMutexW(None, false, PCWSTR::from_raw(wide.as_ptr())) }.map_err(|error| {
            BusinessError::with_message(ErrorCode::InternalError, format!("创建命名互斥锁失败：{error}"))
        })?;
        Ok(Self { handle })
    }

    /// 等待取得所有权（毫秒级超时）。
    pub fn wait(&self, timeout_ms: u32) -> Result<WaitResult, BusinessError> {
        // SAFETY: handle 由 Self 保证有效。
        let result = unsafe { WaitForSingleObject(self.handle, timeout_ms) };
        if result == WAIT_OBJECT_0 {
            Ok(WaitResult::Acquired)
        } else if result == WAIT_TIMEOUT {
            Ok(WaitResult::Timeout)
        } else if result == WAIT_ABANDONED {
            Ok(WaitResult::Abandoned)
        } else {
            Err(BusinessError::with_message(
                ErrorCode::InternalError,
                format!("等待命名互斥锁失败：{result:?}"),
            ))
        }
    }

    /// 释放所有权。
    pub fn release(&self) -> Result<(), BusinessError> {
        // SAFETY: handle 由 Self 保证有效且当前线程持有所有权。
        unsafe { ReleaseMutex(self.handle) }.map_err(|error| {
            BusinessError::with_message(ErrorCode::InternalError, format!("释放命名互斥锁失败：{error}"))
        })
    }
}

impl Drop for NamedMutex {
    fn drop(&mut self) {
        // SAFETY: handle 即将不再使用。
        let _ = unsafe { CloseHandle(self.handle) };
    }
}

// SAFETY: Windows 内核句柄本身线程安全（CreateMutexW/WaitForSingleObject/ReleaseMutex
// 均可跨线程调用），句柄值仅为裸指针故需显式声明 Send/Sync。
unsafe impl Send for NamedMutex {}
unsafe impl Sync for NamedMutex {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    fn unique_name(label: &str) -> String {
        format!(
            r"Local\MemStack.Test.Mutex.{label}.{}.{:?}",
            std::process::id(),
            thread::current().id()
        )
    }

    #[test]
    fn same_name_mutex_serializes_two_threads() {
        let name = unique_name("serialize");
        let mutex = Arc::new(NamedMutex::create(&name).unwrap());
        assert_eq!(mutex.wait(1000).unwrap(), WaitResult::Acquired);

        let waiter = Arc::clone(&mutex);
        let held = thread::spawn(move || {
            // 已被主线程持有：短暂等待必须超时。
            assert_eq!(waiter.wait(50).unwrap(), WaitResult::Timeout);
        });
        held.join().unwrap();

        mutex.release().unwrap();
        // 释放后可再次取得。
        assert_eq!(mutex.wait(1000).unwrap(), WaitResult::Acquired);
        mutex.release().unwrap();
    }

    #[test]
    fn abandoned_mutex_is_taken_over() {
        let name = unique_name("abandoned");
        let main_mutex = NamedMutex::create(&name).unwrap();

        // 子线程取得所有权后直接退出（不释放、不关闭）→ 互斥锁被弃置。
        let takeover_name = name.clone();
        thread::spawn(move || {
            let child = NamedMutex::create(&takeover_name).unwrap();
            assert_eq!(child.wait(1000).unwrap(), WaitResult::Acquired);
            std::mem::forget(child);
        })
        .join()
        .unwrap();

        assert_eq!(main_mutex.wait(5000).unwrap(), WaitResult::Abandoned);
        // 弃置接管后即持有所有权，可正常释放。
        main_mutex.release().unwrap();
    }
}
