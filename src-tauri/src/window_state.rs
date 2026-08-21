//! 窗口状态持久化：与 C# `DesktopWindowState` 同文件同格式无缝迁移。
//!
//! 文件：`%LOCALAPPDATA%\MemStack\window-state.json`。
//! 格式：camelCase 五字段 `{"left","top","width","height","isMaximized"}`
//! （C# `JsonSerializerDefaults.Web` 序列化 `WindowPlacement` record）。
//! 单位：WPF 设备无关像素（DIP），Tauri 侧按 `scale_factor` 与物理像素互转。
//! 恢复规则（对齐 C#）：文件缺失、非法 JSON 或尺寸低于最小值（1080×720）时忽略。

use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// 窗口最小宽度（DIP），与 C# 主窗口 MinWidth 一致。
pub const MIN_WIDTH: f64 = 1080.0;
/// 窗口最小高度（DIP），与 C# 主窗口 MinHeight 一致。
pub const MIN_HEIGHT: f64 = 720.0;

/// 可序列化的窗口位置数据（字段与 C# `WindowPlacement` record 一一对应）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowPlacement {
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
    pub is_maximized: bool,
}

impl WindowPlacement {
    /// 尺寸是否达到最小窗口要求（对齐 C# Restore 的无效即忽略规则）。
    pub fn is_valid(&self) -> bool {
        self.width >= MIN_WIDTH && self.height >= MIN_HEIGHT
    }
}

/// 返回 window-state.json 路径（不创建目录）。
pub fn state_path() -> io::Result<PathBuf> {
    Ok(memory_platform::local_app_data_dir()?.join("window-state.json"))
}

/// 从生产路径读取窗口状态；文件缺失、非法 JSON 或反序列化失败返回 None（容错对齐 C#）。
pub fn load() -> Option<WindowPlacement> {
    let path = state_path().ok()?;
    load_from(&path)
}

/// 把窗口状态写入生产路径（静默容错：IO 失败不阻断主流程）。
pub fn save(placement: &WindowPlacement) {
    if let Ok(path) = state_path() {
        save_to(&path, placement);
    }
}

/// 从指定路径读取窗口状态（非法 JSON 容错）。
pub fn load_from(path: &std::path::Path) -> Option<WindowPlacement> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// 把窗口状态写入指定路径（自动创建父目录；静默容错）。
pub fn save_to(path: &std::path::Path, placement: &WindowPlacement) {
    let Some(directory) = path.parent() else { return };
    if std::fs::create_dir_all(directory).is_err() {
        return;
    }
    if let Ok(text) = serde_json::to_string(placement) {
        let _ = std::fs::write(path, text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placement() -> WindowPlacement {
        WindowPlacement {
            left: 120.5,
            top: 64.0,
            width: 1440.0,
            height: 920.0,
            is_maximized: false,
        }
    }

    #[test]
    fn roundtrip_preserves_camel_case_fields() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("window-state.json");
        save_to(&path, &placement());
        // 文件内容与 C# 格式逐字段一致（camelCase 五字段）。
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"left\":120.5"));
        assert!(text.contains("\"top\":64.0"));
        assert!(text.contains("\"width\":1440.0"));
        assert!(text.contains("\"height\":920.0"));
        assert!(text.contains("\"isMaximized\":false"));
        assert_eq!(load_from(&path), Some(placement()));
    }

    #[test]
    fn reads_csharp_written_state_directly() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("window-state.json");
        // C# Web 序列化输出形态（含缩进与转义差异均可解析）。
        std::fs::write(
            &path,
            r#"{"left":80,"top":40,"width":1600,"height":1000,"isMaximized":true}"#,
        )
        .unwrap();
        let loaded = load_from(&path).unwrap();
        assert!(loaded.is_maximized);
        assert_eq!(loaded.width, 1600.0);
        assert!(loaded.is_valid());
    }

    #[test]
    fn invalid_json_is_tolerated_as_none() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("window-state.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(load_from(&path), None);
        // 文件缺失同样返回 None。
        assert_eq!(load_from(&temp.path().join("absent.json")), None);
    }

    #[test]
    fn undersized_placement_is_invalid() {
        let mut small = placement();
        small.width = 1079.0;
        assert!(!small.is_valid());
        small.width = 1080.0;
        small.height = 719.5;
        assert!(!small.is_valid());
        small.height = 720.0;
        assert!(small.is_valid());
    }

    #[test]
    fn save_creates_missing_directory() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("nested").join("dir").join("window-state.json");
        save_to(&path, &placement());
        assert!(path.is_file());
    }
}
