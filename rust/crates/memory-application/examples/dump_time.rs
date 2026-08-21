use rusqlite::Connection;
fn main() {
    let connection = Connection::open(std::env::args().nth(1).unwrap()).unwrap();
    let mut s = connection
        .prepare("SELECT id,name,created_at,updated_at,workspace_key FROM project ORDER BY created_at DESC LIMIT 6;")
        .unwrap();
    for r in s
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })
        .unwrap()
    {
        let (id, name, c, u, k) = r.unwrap();
        println!("P {c} | {u} | {name} | key={k:?} | {id}");
    }
    let mut s = connection
        .prepare("SELECT id,project_id,created_at,created_source FROM memory ORDER BY created_at DESC LIMIT 5;")
        .unwrap();
    for r in s
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .unwrap()
    {
        let (id, p, c, src) = r.unwrap();
        println!("M {c} | proj={p:?} | src={src} | {id}");
    }
}
