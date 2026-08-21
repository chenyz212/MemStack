
import sqlite3, json

db_paths = [
    r"E:\AICoding\mcp-ai-memory\desktop-client\testdata\tmp-local-copy\desktop-memory.db",
    r"E:\AICoding\mcp-ai-memory\desktop-client\testdata\db-samples\full-sample.db"
]

for db_path in db_paths:
    print("
========== DB:", db_path.split(chr(92))[-1], "==========")
    try:
        conn = sqlite3.connect(db_path)
        cur = conn.cursor()
        
        # 节点数
        cur.execute("SELECT COUNT(*) FROM memory")
        mem_count = cur.fetchone()[0]
        print(f"节点数 (memory rows): {mem_count}")
        
        # 节点详情（标题和类型）
        cur.execute("SELECT id, title, memory_type, importance FROM memory ORDER BY created_at DESC")
        rows = cur.fetchall()
        print("
节点详情：")
        for r in rows:
            print(f"  - {r[1]} [type={r[2]}, imp={r[3]}] id={r[0][:8]}...")
        
        # 关系表 memory_edge 结构和计数
        try:
            cur.execute("SELECT COUNT(*) FROM memory_edge")
            edge_count = cur.fetchone()[0]
            print(f"
关系数 (memory_edge rows): {edge_count}")
            
            # memory_edge 列
            cur.execute("SELECT * FROM memory_edge LIMIT 1")
            cols = [d[0] for d in cur.description]
            print(f"memory_edge列: {cols}")
            
            if edge_count > 0:
                # 显示前几条关系（含强度weight/strength）
                cur.execute(f"SELECT * FROM memory_edge LIMIT 10")
                edges = cur.fetchall()
                print("
关系明细（前10条）：")
                for e in edges:
                    print("  ", json.dumps(dict(zip(cols, [str(x)[:60] for x in e])), ensure_ascii=False))
        except Exception as e:
            print(f"memory_edge ERR: {e}")
        
        # app_setting 中的关系强度阈值或图谱设置
        try:
            cur.execute("SELECT setting_key, setting_value FROM app_setting")
            settings = cur.fetchall()
            if settings:
                print("
app_setting：")
                for k, v in settings:
                    if 'graph' in k.lower() or 'edge' in k.lower() or 'threshold' in k.lower() or 'strength' in k.lower() or 'relation' in k.lower():
                        print(f"  [GRAPH] {k} = {v}")
                    else:
                        print(f"  {k} = {str(v)[:80]}")
        except Exception as e:
            print(f"app_setting ERR: {e}")
        
        conn.close()
    except Exception as e:
        print("ERROR:", e)
