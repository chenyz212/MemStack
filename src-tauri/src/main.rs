//! MemStack 桌面客户端入口：Tauri 壳 + 全部业务 Command（阶段 6/7）。
//!
//! 启动顺序（总计划 §11.1）：单实例锁（plugin，须最先注册）→ 路径解析 →
//! 数据库迁移/schema（AppState::build）→ 注册 Commands → setup 创建主窗口
//! （WebView2 数据目录 + 窗口状态恢复）与托盘 → 启动 Embedding Worker → 加载 Vue。
//!
//! 桌面行为对齐 C# 版（App.xaml.cs）：
//! - 关闭窗口 → 隐藏到托盘并保存窗口状态（保留 WebView 进程，恢复零延迟）；
//! - 托盘菜单「打开 MemStack / 退出」+ 左键单击恢复；tooltip「MemStack」；
//! - 统一退出：隐藏窗口与托盘 → 停 Worker → 保存窗口状态 → exit(0)，
//!   并启动 5s 退出保护线程兜底（对齐 C# ExitGuard）；
//! - 启动失败：中文摘要写 startup-error.log + 原生 MessageBox 弹窗（对齐 C#
//!   WriteStartupFailure + MessageBox 行为）；成功启动后清理旧残留日志。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod error;
mod startup;
mod state;
mod window_state;

use tauri::Manager;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, WebviewUrl, WebviewWindowBuilder, Window, WindowEvent};

use state::AppState;

/// 主窗口标签。
const MAIN_WINDOW: &str = "main";
/// 托盘标识。
const TRAY_ID: &str = "memstack-tray";

fn main() {
    // ---- 品牌升级目录迁移（必须在任何 local_app_data_dir() 调用前执行）----
    // 0.4.0 起 UnifiedAiMemory → MemStack；旧目录存在则整体重命名迁移。
    if let Err(error) = memory_platform::migrate_legacy_dir_if_needed() {
        fail_fast(&format!("{error}（请退出旧版本后重启）"));
    }
    // ---- 路径解析 + 日志轮换 + 首启备份 + 数据库装配（任何失败：日志 + 中文弹窗 + 退出）----
    // §16 启动时日志轮换：失败静默（诊断辅助不阻断启动）。
    memory_platform::log_rotation::rotate_logs();
    let database_path = match state::resolve_database_path() {
        Ok(path) => path,
        Err(error) => fail_fast(&format!("解析数据库路径失败：{error}")),
    };
    // §17.1 首次运行备份：生产数据目录首次被 Rust 版写入前创建一致性快照
    //（MEMSTACK_DB_PATH 显式覆盖的测试/样本场景跳过）。
    if uses_production_database_path()
        && let Err(error) = memory_storage::ensure_first_run_backup(&database_path)
    {
        fail_fast(&format!("创建首次运行备份失败：{error}"));
    }
    let app_state = match AppState::build(database_path.clone()) {
        Ok(state) => state,
        // 带上解析出的库路径：环境变量重定向/沙箱影子目录问题一眼可见。
        Err(error) => fail_fast(&format!("打开数据库失败（{}）：{error}", database_path.display())),
    };

    let result = tauri::Builder::default()
        // 单实例插件必须最先注册：二次启动时唤醒既有主窗口后立即退出。
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            activate_main_window(app);
        }))
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            commands::projects::list_projects,
            commands::projects::create_project,
            commands::projects::update_project,
            commands::projects::archive_project,
            commands::projects::restore_project,
            commands::projects::delete_project_permanent,
            commands::projects::bind_project_workspace,
            commands::projects::unbind_project_workspace,
            commands::workspaces::resolve_workspace,
            commands::workspaces::store_workspace_memory,
            commands::workspaces::get_overview,
            commands::workspaces::get_data_version,
            commands::memory::list_memories,
            commands::memory::get_memory,
            commands::memory::create_memory,
            commands::memory::quick_capture_memory,
            commands::memory::update_memory,
            commands::memory::archive_memory,
            commands::memory::restore_memory,
            commands::memory::delete_memory_permanently,
            commands::memory::list_memory_revisions,
            commands::memory::restore_memory_revision,
            commands::memory::get_memory_facets,
            commands::candidates::list_memory_candidates,
            commands::candidates::update_memory_candidate,
            commands::candidates::confirm_memory_candidate,
            commands::candidates::reject_memory_candidate,
            commands::search::search_memories,
            commands::search::build_memory_context,
            commands::graph::get_global_graph,
            commands::graph::get_neighborhood_graph,
            commands::graph::rebuild_graph,
            commands::embedding::get_embedding_settings,
            commands::embedding::test_embedding_settings,
            commands::embedding::save_embedding_settings,
            commands::embedding::rebuild_embeddings,
            commands::embedding::get_embedding_status,
            commands::mcp::list_mcp_clients,
            commands::mcp::get_mcp_client,
            commands::mcp::create_mcp_client,
            commands::mcp::update_mcp_client,
            commands::mcp::rotate_mcp_client_session_id,
            commands::mcp::revoke_mcp_client,
            commands::mcp::delete_mcp_client,
            commands::mcp::reveal_mcp_client_secret,
            commands::mcp::check_mcp_client_connection,
            commands::mcp::test_mcp_client,
            commands::mcp::get_mcp_connection,
            commands::mcp::get_mcp_client_config_preview,
            commands::mcp::register_mcp_client_config,
            commands::mcp::get_mcp_client_path_health,
            get_app_info,
        ])
        .setup(|app| {
            create_main_window(app)?;
            create_tray(app)?;
            // Embedding Worker：桌面进程是唯一消费者（§10.1）。
            app.state::<AppState>().spawn_worker();
            // 启动成功：清理上次失败残留日志（对齐 C# ClearStartupFailure）。
            startup::clear_startup_failure();
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                // 关闭到托盘：阻止销毁、保存状态并隐藏（保留渲染进程零延迟恢复）。
                api.prevent_close();
                persist_window_state(window);
                let _ = window.hide();
            }
        })
        .build(tauri::generate_context!());

    match result {
        Ok(app) => app.run(|_app_handle, _event| {}),
        Err(error) => fail_fast(&describe_build_failure(&error)),
    }
}

/// 示例 Command：产品信息与 schema 版本（保留自第一轮，供诊断页使用）。
#[tauri::command]
fn get_app_info() -> serde_json::Value {
    serde_json::json!({
        "productName": "MemStack",
        "productVersion": env!("CARGO_PKG_VERSION"),
        "schemaVersion": memory_storage::schema::SUPPORTED_SCHEMA_VERSION,
    })
}

/// 是否走生产数据目录解析（`MEMSTACK_DB_PATH` 为空时为真；测试/样本场景显式覆盖）。
fn uses_production_database_path() -> bool {
    std::env::var("MEMSTACK_DB_PATH").map_or(true, |value| value.trim().is_empty())
}

/// 启动失败统一出口：写日志 → 中文 MessageBox → 退出。
fn fail_fast(summary: &str) -> ! {
    startup::write_startup_failure(summary);
    if let Ok(path) = startup::startup_error_log_path() {
        show_message_box(&format!(
            "MemStack 启动失败\n\n{summary}\n\n详细信息见日志：{}",
            path.display()
        ));
    } else {
        show_message_box(&format!("MemStack 启动失败\n\n{summary}"));
    }
    std::process::exit(1);
}

/// 把 Tauri 构建错误转换为中文摘要；WebView2 缺失时附安装指引。
fn describe_build_failure(error: &tauri::Error) -> String {
    let message = error.to_string();
    if message.to_lowercase().contains("webview") {
        format!(
            "初始化窗口失败：{message}\n\n可能未安装 Microsoft Edge WebView2 Runtime，\
             请从微软官网安装后重试。"
        )
    } else {
        format!("初始化应用失败：{message}")
    }
}

/// 原生中文错误弹窗（启动早期能用；对齐 C# MessageBox 行为）。
fn show_message_box(text: &str) {
    use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    use windows::core::PCWSTR;
    let title: Vec<u16> = "MemStack".encode_utf16().chain(std::iter::once(0)).collect();
    let body: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY：两个宽字符串均以 NUL 结尾。
    unsafe {
        MessageBoxW(
            None,
            PCWSTR::from_raw(body.as_ptr()),
            PCWSTR::from_raw(title.as_ptr()),
            MB_OK | MB_ICONERROR,
        );
    }
}

/// 创建主窗口：WebView2 用户数据目录固定在 LocalAppData，恢复 C# 窗口状态。
fn create_main_window(app: &tauri::App) -> Result<(), tauri::Error> {
    let placement = window_state::load().filter(|placement| placement.is_valid());
    let mut builder = WebviewWindowBuilder::new(app, MAIN_WINDOW, WebviewUrl::default())
        .title("MemStack")
        .min_inner_size(window_state::MIN_WIDTH, window_state::MIN_HEIGHT);

    // 禁止 WebView 数据写入发布目录（绿色版），固定到 %LOCALAPPDATA%\MemStack\webview。
    if let Some(directory) = webview_data_directory() {
        builder = builder.data_directory(directory);
    }

    match &placement {
        Some(saved) => {
            builder = builder
                .inner_size(saved.width, saved.height)
                .position(saved.left, saved.top);
        }
        None => builder = builder.inner_size(1440.0, 920.0).center(),
    }
    let window = builder.build()?;
    if placement.is_some_and(|saved| saved.is_maximized) {
        window.maximize()?;
    }
    Ok(())
}

/// WebView2 用户数据目录：`%LOCALAPPDATA%\MemStack\webview`（目录不可解析时返回 None 交由 Tauri 默认）。
fn webview_data_directory() -> Option<std::path::PathBuf> {
    let directory = memory_platform::local_app_data_dir()
        .map(|base| base.join("webview"))
        .ok()?;
    std::fs::create_dir_all(&directory).ok().map(|()| directory)
}

/// 创建托盘：菜单「打开 MemStack / 退出」，左键单击恢复，tooltip「MemStack」。
fn create_tray(app: &tauri::App) -> Result<(), tauri::Error> {
    let open = MenuItem::with_id(app, "tray-open", "打开 MemStack", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "tray-quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &quit])?;

    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("MemStack")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "tray-open" => activate_main_window(app),
            "tray-quit" => exit_app(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                activate_main_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

/// 显示并置前主窗口（单实例唤醒 / 托盘打开共用路径）。
fn activate_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// 统一退出路径：隐藏窗口与托盘 → 停 Worker → 保存窗口状态 → exit(0)。
///
/// 先启动 5s 退出保护线程兜底（对齐 C# ExitGuard：清理超时也保证进程退出）。
fn exit_app(app: &AppHandle) {
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(5));
        std::process::exit(0);
    });
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        let _ = window.hide();
    }
    let _ = app.remove_tray_by_id(TRAY_ID);
    // Worker 线程 100ms 分片睡眠 + 短事务，正常在 3s 预算内退出。
    app.state::<AppState>().shutdown_worker();
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        persist_webview_state(&window);
    }
    app.exit(0);
}

/// 保存窗口状态（窗口事件回调入口；物理像素 → DIP 与 C# 对齐）。
fn persist_window_state(window: &Window) {
    let placement = build_placement(
        window.is_maximized().unwrap_or(false),
        window.scale_factor().unwrap_or(1.0),
        window
            .inner_size()
            .ok()
            .map(|size| (size.width as f64, size.height as f64)),
        window
            .outer_position()
            .ok()
            .map(|position| (position.x as f64, position.y as f64)),
    );
    window_state::save(&placement);
}

/// 保存 WebviewWindow 状态（退出路径入口，同一转换逻辑）。
fn persist_webview_state(window: &tauri::WebviewWindow) {
    let placement = build_placement(
        window.is_maximized().unwrap_or(false),
        window.scale_factor().unwrap_or(1.0),
        window
            .inner_size()
            .ok()
            .map(|size| (size.width as f64, size.height as f64)),
        window
            .outer_position()
            .ok()
            .map(|position| (position.x as f64, position.y as f64)),
    );
    window_state::save(&placement);
}

/// 物理像素 → DIP 组装窗口状态；最大化时保留上次保存的正常态 bounds
/// （等价 C# RestoreBounds 语义）。
fn build_placement(
    maximized: bool,
    scale: f64,
    size: Option<(f64, f64)>,
    position: Option<(f64, f64)>,
) -> window_state::WindowPlacement {
    let (width, height) = size
        .map(|(width, height)| (width / scale, height / scale))
        .unwrap_or((1440.0, 920.0));
    let (left, top) = position
        .map(|(left, top)| (left / scale, top / scale))
        .unwrap_or((100.0, 100.0));
    if maximized {
        match window_state::load() {
            Some(previous) if previous.is_valid() => window_state::WindowPlacement {
                is_maximized: true,
                ..previous
            },
            _ => window_state::WindowPlacement {
                left,
                top,
                width,
                height,
                is_maximized: true,
            },
        }
    } else {
        window_state::WindowPlacement {
            left,
            top,
            width,
            height,
            is_maximized: false,
        }
    }
}
