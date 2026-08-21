//! 阶段 2 高风险验证：C# 加密的 DPAPI 黄金数据由 Rust 解密（跨语言单向兼容）。
//!
//! 黄金数据绑定生成时的 Windows 用户与机器；文件缺失时跳过。

use std::path::PathBuf;

fn golden_path() -> Option<PathBuf> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../testdata/golden/dpapi-golden.json");
    path.exists().then_some(path)
}

#[derive(serde::Deserialize)]
struct GoldenCase {
    name: String,
    plaintext: String,
    ciphertext: String,
}

#[derive(serde::Deserialize)]
struct GoldenDocument {
    entropy: String,
    cases: Vec<GoldenCase>,
}

#[test]
fn decrypts_csharp_dpapi_golden_data() {
    let Some(path) = golden_path() else {
        eprintln!("跳过：缺少 dpapi-golden.json（C# 侧生成）");
        return;
    };
    let document: GoldenDocument = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(document.entropy, "MemStack.Desktop.0.4.0");
    assert!(!document.cases.is_empty(), "黄金数据必须覆盖至少一条用例");
    for case in &document.cases {
        let plaintext = memory_platform::unprotect(&case.ciphertext)
            .unwrap_or_else(|error| panic!("用例 {} 解密失败：{error}", case.name));
        assert_eq!(plaintext, case.plaintext, "用例 {} 明文不匹配", case.name);
    }
}
