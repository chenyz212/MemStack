//! 桌面生命周期集成测试（第四轮 T10/T11）：
//! - 双实例：第二个实例应立即退出（退出码 0），首个实例存活；
//! - 端口：应用运行期间不监听 18461 / 10212（「可绑定 = 无监听」，等价 netstat 验收）；
//! - 启动失败：数据库不可打开时写 `startup-error.log`（中文摘要 + UTC 时间戳）并停留弹窗。
//!
//! 运行前提：本机没有已启动的忆栈桌面进程（单实例插件按应用标识判定，避免误判）。
//! 两个测试通过共享锁串行执行。

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// 桌面 exe（cargo 为集成测试自动构建的同包二进制）。
const EXE: &str = env!("CARGO_BIN_EXE_memstack-desktop");
/// 主窗口标题。
const MAIN_TITLE: &str = "MemStack";
/// 本轮废除的两个端口（C# 版 REST 18461 / MCP 10212）。
const RETIRED_PORTS: [u16; 2] = [18461, 10212];

/// 串行锁：两个测试都独占「MemStack 桌面进程」身份。
fn lifecycle_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// 是否已有 MemStack 桌面进程在运行（tasklist 名称过滤）。
fn desktop_exes_running() -> bool {
    let output = Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq memstack-desktop.exe", "/NH"])
        .output()
        .expect("执行 tasklist 失败");
    let text = String::from_utf8_lossy(&output.stdout).to_lowercase();
    text.contains("memstack-desktop.exe")
}

/// 轮询等待条件成立（固定 100ms 间隔）。
fn wait_until<F: FnMut() -> bool>(timeout: Duration, mut condition: F) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    condition()
}

/// 主窗口「MemStack」是否已出现（WebView2 就绪 ⇒ 单实例插件已初始化）。
fn find_main_window() -> bool {
    use windows::Win32::UI::WindowsAndMessaging::FindWindowW;
    use windows::core::PCWSTR;
    let title: Vec<u16> = MAIN_TITLE.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY：title 以 NUL 结尾。
    unsafe { FindWindowW(None, PCWSTR::from_raw(title.as_ptr())).is_ok() }
}

/// 启动桌面 exe（数据库指向指定路径，隔离生产库）。
fn spawn_desktop(database_path: &Path) -> Child {
    Command::new(EXE)
        .env("MEMSTACK_DB_PATH", database_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("启动桌面 exe 失败")
}

/// 等待子进程退出并返回退出码（超时返回 None）。
fn wait_exit(child: &mut Child, timeout: Duration) -> Option<i32> {
    let exited = wait_until(timeout, || child.try_wait().expect("查询子进程状态失败").is_some());
    if exited {
        child.wait().expect("等待子进程退出失败").code()
    } else {
        None
    }
}

/// 结束进程树（含 WebView2 子进程）。
fn kill_tree(pid: u32) {
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// 断言退役端口无监听：绑定成功即代表无任何进程占用（等价 netstat 为空）。
fn assert_ports_free() {
    for port in RETIRED_PORTS {
        let bindable = TcpListener::bind(("127.0.0.1", port)).is_ok();
        assert!(bindable, "端口 {port} 仍被监听（本轮应彻底废除 18461/10212）");
    }
}

/// 生产 startup-error.log 路径（与 memory_platform::logs_dir 同源）。
fn startup_log_path() -> PathBuf {
    let local_app_data = std::env::var("LOCALAPPDATA").expect("缺少 LOCALAPPDATA 环境变量");
    Path::new(&local_app_data)
        .join("MemStack")
        .join("logs")
        .join("startup-error.log")
}

#[test]
fn second_instance_exits_zero_and_first_survives() {
    let _guard = lifecycle_lock().lock().unwrap();
    assert!(
        !desktop_exes_running(),
        "检测到已运行的 MemStack 桌面进程：请先关闭后再执行本测试（单实例按应用标识判定）"
    );

    let temp = tempfile::tempdir().unwrap();
    let mut first = spawn_desktop(&temp.path().join("lifecycle.db"));

    // 等待主窗口出现（WebView2 初始化完成 ⇒ 单实例插件已就绪）。
    let window_ready = wait_until(Duration::from_secs(30), find_main_window);
    if !window_ready {
        kill_tree(first.id());
        panic!("主窗口 30 秒内未出现（首个实例启动失败）");
    }

    // 端口验收：运行期间不监听 18461 / 10212。
    assert_ports_free();

    // 二次启动：应触发单实例回调（唤醒首个实例）并立即退出，退出码 0。
    let mut second = spawn_desktop(&temp.path().join("lifecycle.db"));
    let second_code = wait_exit(&mut second, Duration::from_secs(20));
    let first_alive = first.try_wait().expect("查询首个实例状态失败").is_none();

    // 无论断言结果如何都先清理首个实例，避免泄漏 GUI 进程。
    kill_tree(first.id());

    assert_eq!(second_code, Some(0), "第二个实例应以退出码 0 结束");
    assert!(first_alive, "首个实例在二次启动后应存活");
}

#[test]
fn startup_failure_writes_chinese_log_and_blocks_on_message_box() {
    let _guard = lifecycle_lock().lock().unwrap();

    // 备份生产日志现状，测试后恢复原状。
    let log = startup_log_path();
    let original = std::fs::read(&log).ok();

    // 数据库路径的父级是一个普通文件 ⇒ 打开必然失败 ⇒ fail_fast 路径。
    let temp = tempfile::tempdir().unwrap();
    let blocker = temp.path().join("blocker.bin");
    std::fs::write(&blocker, b"blocker").unwrap();
    let mut child = spawn_desktop(&blocker.join("memory.db"));

    // fail_fast 先写日志再弹窗：日志出现即进入失败路径。
    let log_written = wait_until(Duration::from_secs(20), || {
        std::fs::read_to_string(&log)
            .map(|content| content.contains("打开数据库失败"))
            .unwrap_or(false)
    });
    // 弹窗为模态：进程应仍存活等待用户确认（而非静默退出）。
    let blocked_on_dialog = child.try_wait().expect("查询子进程状态失败").is_none();

    kill_tree(child.id());

    // 恢复日志原状（存在则还原内容，不存在则删除测试残留）。
    match &original {
        Some(bytes) => std::fs::write(&log, bytes).expect("恢复 startup-error.log 失败"),
        None => {
            let _ = std::fs::remove_file(&log);
        }
    }

    assert!(log_written, "startup-error.log 应含中文摘要「打开数据库失败」");
    assert!(blocked_on_dialog, "启动失败应停留于中文弹窗等待用户确认");
}
