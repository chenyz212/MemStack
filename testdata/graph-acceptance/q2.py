import sqlite3, json
db_paths = [
  r"E:\AICoding\mcp-ai-memory\desktop-client\testdata\tmp-local-copy\desktop-memory.db",
  r"E:\AICoding\mcp-ai-memory\desktop-client\testdata\db-samples\full-sample.db"
]
for db_path in db_paths:
    print("DB:", db_path.split(chr(92))[-1])
    conn = sqlite3.connect(db_path)
    cur = conn.cursor()
    cur.execute("SELECT COUNT(*) FROM memory")
    print("  节点数:", cur.fetchone()[0])
    try:
        cur.execute("SELECT COUNT(*) FROM memory_edge")
        ec = cur.fetchone()[0]
        print("  关系数(memory_edge):", ec)
        cur.execute("PRAGMA table_info(memory_edge)")
        cols = cur.fetchall()
        print("  memory_edge列:", [c[1] for c in cols])
        if ec > 0:
            col_names = [c[1] for c in cols]
            cur.execute("SELECT * FROM memory_edge LIMIT 5")
            for row in cur.fetchall():
                d = dict(zip(col_names, [str(x)[:60] for x in row]))
                print("    EDGE:", json.dumps(d, ensure_ascii=False))
    except Exception as e:
        print("  edge ERR:", e)
    try:
        cur.execute("SELECT setting_key, setting_value FROM app_setting")
        for k, v in cur.fetchall():
            kl = k.lower()
            mark = " [GRAPH]" if any(w in kl for w in ["graph","edge","threshold","strength","relation"]) else ""
            print("  setting" + mark + ":", k, "=", str(v)[:80])
    except Exception as e:
        print("  setting ERR:", e)
    cur.execute("SELECT id, title, memory_type, importance FROM memory ORDER BY created_at DESC")
    for r in cur.fetchall():
        print("  node:", r[1], "type="+str(r[2]), "imp="+str(r[3]))
    conn.close()
