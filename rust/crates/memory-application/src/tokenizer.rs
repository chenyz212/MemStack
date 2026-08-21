//! 中文检索分词器：移植 C# `ChineseSearchTokenizer`。
//!
//! 规则与 C# 完全一致：
//! 1. NFKC 归一化 + 小写。
//! 2. 中文（U+3400..U+9FFF 的表意文字）生成单字和相邻二元词组。
//! 3. 拉丁字母、数字和 `_ - . / \` 连成完整词元。
//! 4. 词元去重后以空格连接，写入 FTS5 token 列。
//!
//! 来源：第一轮在 memstack-mcp-stdio 内实现并验证；第二轮迁入本 crate
//! 作为唯一实现（MCP app 改为依赖引用，避免双实现漂移）。

use std::collections::HashSet;

/// 判断字符是否属于 C# 版本的中文区间（UnicodeCategory.OtherLetter 且 0x3400..=0x9FFF）。
fn is_chinese(character: char) -> bool {
    // U+3400..=0x4DBF（扩展A）与 0x4E00..=0x9FFF（基本区）在该区间内全部是 OtherLetter。
    ('\u{3400}'..='\u{4DBF}').contains(&character) || ('\u{4E00}'..='\u{9FFF}').contains(&character)
}

/// 标准化文本并生成中文单字、二元词组和拉丁词元，等价于 C# `Tokenize`。
pub fn tokenize(text: &str) -> String {
    use unicode_normalization::UnicodeNormalization;

    let normalized: String = text.nfkc().flat_map(|character| character.to_lowercase()).collect();
    let mut tokens: HashSet<String> = HashSet::new();
    let mut latin_token = String::new();
    let mut chinese_run: Vec<char> = Vec::new();

    for character in normalized.chars() {
        if is_chinese(character) {
            flush_latin_token(&mut latin_token, &mut tokens);
            chinese_run.push(character);
            continue;
        }
        flush_chinese_run(&mut chinese_run, &mut tokens);
        if character.is_alphanumeric() || matches!(character, '_' | '-' | '.' | '/' | '\\') {
            latin_token.push(character);
        } else {
            flush_latin_token(&mut latin_token, &mut tokens);
        }
    }
    flush_chinese_run(&mut chinese_run, &mut tokens);
    flush_latin_token(&mut latin_token, &mut tokens);
    tokens.into_iter().collect::<Vec<_>>().join(" ")
}

/// 把连续中文转换为单字和相邻二元词组。
fn flush_chinese_run(run: &mut Vec<char>, tokens: &mut HashSet<String>) {
    for index in 0..run.len() {
        tokens.insert(run[index].to_string());
        if index + 1 < run.len() {
            tokens.insert(format!("{}{}", run[index], run[index + 1]));
        }
    }
    run.clear();
}

/// 保存一个完整的拉丁、数字或路径词元。
fn flush_latin_token(token: &mut String, tokens: &mut HashSet<String>) {
    if !token.is_empty() {
        tokens.insert(token.clone());
        token.clear();
    }
}

/// 将词元转成 FTS5 MATCH 表达式（与 C# `BuildMatchExpression` 一致）：
/// 中文词元精确匹配，含 ASCII 字母/数字的词元前缀匹配。
pub fn build_match_expression(tokenized: &str) -> String {
    tokenized
        .split(' ')
        .filter(|token| !token.is_empty())
        .map(|token| {
            let quoted = format!("\"{}\"", token.replace('"', "\"\""));
            let has_ascii_alnum = token
                .chars()
                .any(|character| character.is_ascii() && (character.is_ascii_alphanumeric() || character == '_'));
            if has_ascii_alnum { format!("{quoted}*") } else { quoted }
        })
        .collect::<Vec<_>>()
        .join(" AND ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chinese_text_produces_unigrams_and_bigrams() {
        let tokens = tokenize("发布验证");
        assert!(tokens.contains("发"));
        assert!(tokens.contains("发布"));
        assert!(tokens.contains("布验"));
        assert!(tokens.contains("验证"));
    }

    #[test]
    fn latin_words_stay_whole() {
        let tokens = tokenize("Rust Tauri migration");
        assert!(tokens.contains("rust"));
        assert!(tokens.contains("tauri"));
        assert!(tokens.contains("migration"));
    }

    #[test]
    fn mixed_content_separates_runs() {
        let tokens = tokenize("忆栈Rust迁移");
        assert!(tokens.contains("忆栈"));
        assert!(tokens.contains("rust"));
        assert!(tokens.contains("迁移"));
    }

    #[test]
    fn match_expression_quotes_and_prefixes() {
        let tokenized = tokenize("发布验证 rust");
        let expression = build_match_expression(&tokenized);
        assert!(expression.contains("\"发布\""));
        assert!(expression.contains("\"rust\"*"));
        assert!(expression.contains(" AND "));
    }
}
