
import sqlite3, json

for db_path in [r"E:\AICoding\mcp-ai-memory\desktop-client\testdata\tmp-local-copy\desktop-memory.db"]:
    print("=== DB:", db_path, "===")
    try:
        conn = sqlite3.connect(db_path)
        cur = conn.cursor()
        # 1. 列出所有表
        cur.execute("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        tables = [r[0] for r in cur.fetchall()]
        print("Tables:", json.dumps(tables, ensure_ascii=False))
        # 2. 每个表的行数
        for t in tables:
            try:
                cur.execute(f"SELECT COUNT(*) FROM [{t}]")
                c = cur.fetchone()[0]
                if c > 0:
                    print(f"  [{t}] rows={c}")
                    # 查看前3列名
                    cur.execute(f"SELECT * FROM [{t}] LIMIT 1")
                    cols = [d[0] for d in cur.description]
                    print(f"    columns: {cols[:15]}")
            except Exception as e:
                print(f"  [{t}] ERR: {e}")
        conn.close()
    except Exception as e:
        print("  ERROR:", e)
