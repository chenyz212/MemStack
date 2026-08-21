//! 临时诊断探针：对 C# 生成的 contract.db 复现 search_tag_filter 的 FTS+tag 查询。

fn count(connection: &rusqlite::Connection, label: &str, sql: &str) {
    let count: i64 = connection.query_row(sql, [], |row| row.get(0)).unwrap_or(-1);
    println!("== {label} = {count}");
}

fn main() {
    let path = std::env::args().nth(1).expect("用法：probe_fts <contract.db 路径>");
    let connection = rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();

    let expression = "\"样\" AND \"样本\" AND \"本\"";
    count(
        &connection,
        "a) 纯 FTS AND（无 rank/JOIN/过滤）",
        &format!("SELECT count(*) FROM memory_fts WHERE memory_fts MATCH '{expression}';"),
    );
    count(
        &connection,
        "b) FTS AND + rank bm25",
        &format!(
            "SELECT count(*) FROM memory_fts WHERE memory_fts MATCH '{expression}' \
             AND rank MATCH 'bm25(5.0,4.0,3.0,1.0)';"
        ),
    );
    count(
        &connection,
        "c) FTS AND + JOIN",
        &format!(
            "SELECT count(*) FROM memory_fts JOIN memory m ON m.id=memory_fts.memory_id \
             WHERE memory_fts MATCH '{expression}';"
        ),
    );
    count(
        &connection,
        "d) FTS AND + status",
        &format!(
            "SELECT count(*) FROM memory_fts JOIN memory m ON m.id=memory_fts.memory_id \
             WHERE memory_fts MATCH '{expression}' AND m.status='Active';"
        ),
    );
    count(
        &connection,
        "e) 完整 C# 形态（无 tag 值）",
        &format!(
            "SELECT count(*) FROM memory_fts JOIN memory m ON m.id=memory_fts.memory_id \
             WHERE memory_fts MATCH '{expression}' AND rank MATCH 'bm25(5.0,4.0,3.0,1.0)' \
             AND m.status='Active';"
        ),
    );
    count(
        &connection,
        "f) 单短语 样本（带 rank）",
        "SELECT count(*) FROM memory_fts WHERE memory_fts MATCH '\"样本\"' AND rank MATCH 'bm25(5.0,4.0,3.0,1.0)';",
    );
    count(
        &connection,
        "g) tags LIKE（C# 转义存储）",
        "SELECT count(*) FROM memory WHERE tags_json LIKE '%' || '筛选标签' || '%';",
    );
    let raw: Option<String> = connection
        .query_row(
            "SELECT tags_json FROM memory WHERE title='标签筛选样本';",
            [],
            |row| row.get(0),
        )
        .ok();
    println!("== 标签筛选样本 tags_json 原文 = {raw:?}");
}
