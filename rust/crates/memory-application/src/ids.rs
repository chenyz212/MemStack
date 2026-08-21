//! 标识生成抽象：注入式 GUID 生成器。
//!
//! 生产用 `GuidGenerator`（uuid v4，小写带连字符，与 C# `Guid.ToString()` 一致）；
//! 测试/契约场景用 `FixedIdGenerator` 让运行期 ID 可预测。

/// 可注入的标识生成器。
pub trait IdGenerator: Send + Sync {
    /// 生成新的 GUID 文本（小写带连字符，8-4-4-4-12）。
    fn new_id(&self) -> String;
}

/// 系统随机 GUID 生成器（生产环境）。
#[derive(Debug, Clone, Copy, Default)]
pub struct GuidGenerator;

impl IdGenerator for GuidGenerator {
    fn new_id(&self) -> String {
        uuid::Uuid::new_v4().to_string()
    }
}

/// 固定序列生成器（测试/契约场景确定化）：按顺序返回预置 ID，耗尽后重复最后一个。
#[derive(Debug)]
pub struct FixedIdGenerator {
    ids: Vec<String>,
    cursor: std::sync::atomic::AtomicUsize,
}

impl FixedIdGenerator {
    /// 预置 ID 序列（至少 1 个）。
    pub fn new(ids: Vec<String>) -> Self {
        assert!(!ids.is_empty(), "FixedIdGenerator 需要至少一个预置 ID");
        Self {
            ids,
            cursor: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

impl IdGenerator for FixedIdGenerator {
    fn new_id(&self) -> String {
        let index = self.cursor.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.ids[index.min(self.ids.len() - 1)].clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guid_generator_matches_csharp_format() {
        let id = GuidGenerator.new_id();
        assert_eq!(id.len(), 36);
        assert_eq!(id.chars().filter(|c| *c == '-').count(), 4);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit() || c == '-'));
        assert!(id.chars().all(|c| !c.is_ascii_uppercase()));
    }

    #[test]
    fn fixed_generator_sequences_then_holds() {
        let generator = FixedIdGenerator::new(vec![
            "11111111-1111-4111-8111-111111111111".to_string(),
            "22222222-2222-4222-8222-222222222222".to_string(),
        ]);
        assert_eq!(generator.new_id(), "11111111-1111-4111-8111-111111111111");
        assert_eq!(generator.new_id(), "22222222-2222-4222-8222-222222222222");
        assert_eq!(generator.new_id(), "22222222-2222-4222-8222-222222222222");
    }
}
