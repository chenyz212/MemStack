//! 时间抽象：注入式时钟与 C# 兼容的存储格式。
//!
//! 服务统一注入 `Clock`（C# 仅注入 TimeProvider；Rust 同语义），
//! 生产用 `SystemClock`，测试/契约场景用 `FixedClock` 确定化输出。

use chrono::{DateTime, Utc};

/// 可注入的时钟。
pub trait Clock: Send + Sync {
    /// 当前 UTC 时间。
    fn now_utc(&self) -> DateTime<Utc>;
}

/// 系统真实时钟（生产环境）。
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_utc(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// 固定时钟（测试/契约场景确定化，可按需推进）。
#[derive(Debug)]
pub struct FixedClock {
    instant: std::sync::Mutex<DateTime<Utc>>,
}

impl FixedClock {
    /// 固定为指定时刻。
    pub fn new(instant: DateTime<Utc>) -> Self {
        Self {
            instant: std::sync::Mutex::new(instant),
        }
    }

    /// 推进固定时刻（契约场景用于消除与 C# 真实时钟的排序差异）。
    pub fn advance(&self, duration: chrono::Duration) {
        *self.instant.lock().expect("FixedClock 已中毒") += duration;
    }
}

impl Clock for FixedClock {
    fn now_utc(&self) -> DateTime<Utc> {
        *self.instant.lock().expect("FixedClock 已中毒")
    }
}

/// 将时间格式化为库内统一存储文本。
///
/// 与 C# `DateTimeOffset.ToString("O")` 的 7 位小数 UTC 形式逐字符一致
/// （如 `2026-08-15T08:00:00.0000000+00:00`），保证排序与跨语言解析兼容。
/// 注意：chrono 的 `%.Nf` 仅支持 3/6/9 位精度，7 位小数手动截断补齐。
pub fn format_storage_time(instant: DateTime<Utc>) -> String {
    use chrono::Timelike;

    let date_part = instant.format("%Y-%m-%dT%H:%M:%S");
    // 纳秒 → 100ns 单位 = 7 位小数（截断而非四舍五入，与 "O" 格式一致）。
    let fraction = instant.nanosecond() / 100;
    format!("{date_part}.{fraction:07}+00:00")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn storage_time_matches_csharp_roundtrip_format() {
        use chrono::Timelike;

        let instant = Utc.with_ymd_and_hms(2026, 8, 15, 8, 0, 0).unwrap();
        assert_eq!(format_storage_time(instant), "2026-08-15T08:00:00.0000000+00:00");
        // 带毫秒：7 位小数完整保留。
        let precise = Utc
            .with_ymd_and_hms(2026, 1, 2, 3, 4, 5)
            .unwrap()
            .with_nanosecond(600_000_000)
            .unwrap();
        assert_eq!(format_storage_time(precise), "2026-01-02T03:04:05.6000000+00:00");
    }

    #[test]
    fn storage_time_sorts_like_csharp_text() {
        // 同格式文本排序 = 时间排序（C# 侧依赖该性质做 keyset 分页）。
        let earlier = format_storage_time(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap());
        let later = format_storage_time(Utc.with_ymd_and_hms(2026, 1, 2, 0, 0, 0).unwrap());
        assert!(earlier < later);
    }

    #[test]
    fn fixed_clock_returns_constant() {
        let instant = Utc.with_ymd_and_hms(2026, 8, 15, 8, 0, 0).unwrap();
        let clock = FixedClock::new(instant);
        assert_eq!(clock.now_utc(), instant);
        assert_eq!(clock.now_utc(), instant);
    }
}
