//! 生产库只读诊断：dump 项目绑定状态与最近记忆的 scope/project 归属。
//! `cargo run --release -p memory-application --example probe_bind -- <db路径>`
//! 仅执行 SELECT，不修改任何数据。

use rusqlite::Connection;

fn main() {
    let path = std::env::args().nth(1).expect("用法：probe_bind <db路径>（只读诊断）");
    let connection = Connection::open(&path).expect("打开数据库失败");
    let mut statement = connection
        .prepare(
            "SELECT id, name, is_archived, workspace_key, workspace_label \
             FROM project ORDER BY created_at;",
        )
        .unwrap();
    println!("== project 表 ==");
    for row in statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })
        .unwrap()
    {
        let (id, name, archived, key, label) = row.unwrap();
        println!("  id={id} name={name:?} archived={archived} key={key:?} label={label:?}");
    }
    let mut statement = connection
        .prepare(
            "SELECT scope, project_id, COUNT(*) FROM memory \
             WHERE status='Active' GROUP BY scope, project_id;",
        )
        .unwrap();
    println!("== 活动记忆归属 ==");
    for row in statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .unwrap()
    {
        let (scope, project_id, count) = row.unwrap();
        println!("  scope={scope} project={project_id:?} count={count}");
    }
}
