//! 进程树：识别 MCP 子进程的客户端祖先进程并监控其存活。
//!
//! 背景：AI 客户端（Trae/TraeWork/WorkBuddy）以子进程方式拉起 MemStack-MCP，
//! 但空闲时可能不发任何 MCP 帧（含 ping）——纯时间判活必然误杀安静的真连接。
//! 正确的判活信号是客户端进程本身：
//! - 启动时用 Toolhelp32 快照沿 PPID 链向上走，跳过 cmd/powershell 等壳进程，
//!   锁定最近的非壳祖先进程（即客户端本体）；
//! - 运行期周期性快照确认该 PID 仍在且进程名一致（防 PID 复用误判存活）。
//!
//! 祖先链不可解析（父进程已退出成断链 / 到达系统常驻进程 / 成环）时返回 `None`，
//! 调用方退化为空闲超时兜底模式。

use std::collections::{BTreeMap, BTreeSet};

use memory_domain::{BusinessError, ErrorCode};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};

/// 快照中的一个进程条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessEntry {
    pub pid: u32,
    pub ppid: u32,
    pub name: String,
}

/// 被监控的客户端祖先进程。
///
/// `name` 为 `None` 时仅按 PID 判存活（测试缝指定的未知进程）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AncestorProcess {
    pub pid: u32,
    pub name: Option<String>,
}

/// 壳进程（小写比较，`.exe` 后缀可选）：客户端经壳包装启动（如 `cmd /c`）时向上跳过。
const SHELL_NAMES: &[&str] = &["cmd", "powershell", "pwsh", "conhost", "bash", "sh"];

/// 系统常驻进程（小写比较）：作为祖先说明链路已脱离用户会话（服务拉起/孤儿场景）。
/// 它们永不退出，若当作监控目标会把本进程挂成僵尸——必须判为不可解析。
const SYSTEM_NAMES: &[&str] = &[
    "system", "registry", "smss", "csrss", "wininit", "winlogon", "services", "lsass", "svchost",
];

/// 进程名归一化：去首尾空白、转小写、去 `.exe` 后缀。
fn normalize_name(name: &str) -> String {
    let lowered = name.trim().to_ascii_lowercase();
    lowered.strip_suffix(".exe").unwrap_or(&lowered).to_string()
}

fn is_shell(name: &str) -> bool {
    SHELL_NAMES.contains(&normalize_name(name).as_str())
}

fn is_system_process(name: &str) -> bool {
    SYSTEM_NAMES.contains(&normalize_name(name).as_str())
}

/// 全量进程快照（Toolhelp32；单次调用毫秒级，供启动解析与周期判活）。
pub fn snapshot_processes() -> Result<Vec<ProcessEntry>, BusinessError> {
    // SAFETY: snapshot 句柄由本函数持有并在枚举结束后关闭。
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
        .map_err(|error| BusinessError::with_message(ErrorCode::InternalError, format!("创建进程快照失败：{error}")))?;
    let mut entries = Vec::new();
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    // SAFETY: entry 已按 dwSize 初始化；snapshot 句柄有效。
    if unsafe { Process32FirstW(snapshot, &mut entry) }.is_ok() {
        loop {
            let end = entry
                .szExeFile
                .iter()
                .position(|&ch| ch == 0)
                .unwrap_or(entry.szExeFile.len());
            entries.push(ProcessEntry {
                pid: entry.th32ProcessID,
                ppid: entry.th32ParentProcessID,
                name: String::from_utf16_lossy(&entry.szExeFile[..end]),
            });
            // SAFETY: 同上。
            if unsafe { Process32NextW(snapshot, &mut entry) }.is_err() {
                break;
            }
        }
    }
    // SAFETY: snapshot 即将不再使用。
    let _ = unsafe { CloseHandle(snapshot) };
    Ok(entries)
}

/// 从 `start_pid` 沿 PPID 链向上找最近的非壳祖先进程（即客户端本体）。
///
/// 返回 `None`：起始进程不在快照、链断（父进程已退出）、到达系统常驻进程或成环。
pub fn resolve_client_ancestor(table: &[ProcessEntry], start_pid: u32) -> Option<AncestorProcess> {
    let by_pid: BTreeMap<u32, &ProcessEntry> = table.iter().map(|entry| (entry.pid, entry)).collect();
    let mut current = *by_pid.get(&start_pid)?;
    let mut visited = BTreeSet::new();
    loop {
        if !visited.insert(current.pid) {
            return None; // 成环（快照竞态或 PID 复用），判不可解析。
        }
        if current.ppid == 0 {
            return None;
        }
        let parent = *by_pid.get(&current.ppid)?;
        if is_system_process(&parent.name) {
            return None;
        }
        if !is_shell(&parent.name) {
            return Some(AncestorProcess {
                pid: parent.pid,
                name: Some(parent.name.clone()),
            });
        }
        current = parent;
    }
}

/// 进程是否存活：PID 不在快照中或进程名不一致（PID 被复用）均视为死亡。
pub fn is_process_alive(table: &[ProcessEntry], pid: u32, name: &str) -> bool {
    table
        .iter()
        .find(|entry| entry.pid == pid)
        .is_some_and(|found| normalize_name(&found.name) == normalize_name(name))
}

impl AncestorProcess {
    /// 在给定快照中检查祖先是否存活；`name` 为 `None` 时仅按 PID 判定。
    pub fn is_alive_in(&self, table: &[ProcessEntry]) -> bool {
        match &self.name {
            Some(expected) => is_process_alive(table, self.pid, expected),
            None => table.iter().any(|entry| entry.pid == self.pid),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(pid: u32, ppid: u32, name: &str) -> ProcessEntry {
        ProcessEntry {
            pid,
            ppid,
            name: name.to_string(),
        }
    }

    #[test]
    fn direct_parent_is_client() {
        let table = [entry(10, 100, "MemStack-MCP.exe"), entry(100, 1, "Trae.exe")];
        let ancestor = resolve_client_ancestor(&table, 10).unwrap();
        assert_eq!(ancestor.pid, 100);
        assert_eq!(ancestor.name.as_deref(), Some("Trae.exe"));
    }

    #[test]
    fn skips_shell_ancestors() {
        let table = [
            entry(10, 20, "MemStack-MCP.exe"),
            entry(20, 100, "cmd.exe"),
            entry(100, 1, "Trae.exe"),
        ];
        let ancestor = resolve_client_ancestor(&table, 10).unwrap();
        assert_eq!(ancestor.pid, 100, "cmd 是壳，必须向上找到 Trae");
    }

    #[test]
    fn system_ancestor_is_unresolvable() {
        // 服务拉起的进程：父链到 svchost 即脱离用户会话，判不可解析（退化兜底模式）。
        let table = [
            entry(10, 20, "MemStack-MCP.exe"),
            entry(20, 666, "cmd.exe"),
            entry(666, 4, "svchost.exe"),
        ];
        assert!(resolve_client_ancestor(&table, 10).is_none());
    }

    #[test]
    fn broken_chain_is_unresolvable() {
        // 父进程 999 已退出不在快照中：链断。
        let table = [entry(10, 999, "MemStack-MCP.exe")];
        assert!(resolve_client_ancestor(&table, 10).is_none());
    }

    #[test]
    fn cycle_is_unresolvable() {
        let table = [
            entry(10, 20, "MemStack-MCP.exe"),
            entry(20, 30, "cmd.exe"),
            entry(30, 20, "cmd.exe"),
        ];
        assert!(resolve_client_ancestor(&table, 10).is_none());
    }

    #[test]
    fn start_pid_missing_is_unresolvable() {
        let table = [entry(100, 1, "Trae.exe")];
        assert!(resolve_client_ancestor(&table, 10).is_none());
    }

    #[test]
    fn root_ppid_zero_is_unresolvable() {
        let table = [entry(10, 0, "MemStack-MCP.exe")];
        assert!(resolve_client_ancestor(&table, 10).is_none());
    }

    #[test]
    fn is_alive_checks_pid_and_name() {
        let table = [entry(100, 1, "Trae.exe")];
        assert!(is_process_alive(&table, 100, "Trae.exe"));
        assert!(
            is_process_alive(&table, 100, "trae"),
            "进程名归一化（.exe 可选、大小写不敏感）"
        );
        assert!(
            !is_process_alive(&table, 100, "OtherApp.exe"),
            "PID 复用后进程名不一致视为死亡"
        );
        assert!(!is_process_alive(&table, 101, "Trae.exe"), "PID 不存在视为死亡");
    }

    #[test]
    fn ancestor_without_name_matches_pid_only() {
        let table = [entry(100, 1, "Whatever.exe")];
        let ancestor = AncestorProcess { pid: 100, name: None };
        assert!(ancestor.is_alive_in(&table));
    }

    #[test]
    fn shell_and_system_name_normalization() {
        assert!(is_shell("CMD.EXE"));
        assert!(is_shell("PowerShell"));
        assert!(!is_shell("Trae.exe"));
        assert!(is_system_process("System"));
        assert!(is_system_process("SVCHOST.EXE"));
        assert!(!is_system_process("Trae.exe"));
    }

    /// 真实快照冒烟：本进程必在快照中，且名字非空。
    #[test]
    fn snapshot_contains_self() {
        let table = snapshot_processes().expect("进程快照应成功");
        let self_entry = table
            .iter()
            .find(|entry| entry.pid == std::process::id())
            .expect("快照必须包含本进程");
        assert!(!self_entry.name.trim().is_empty());
    }
}
