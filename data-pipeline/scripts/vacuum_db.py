"""数据库 VACUUM 脚本
重建数据库文件，回收删除/更新产生的碎片空间。
"""
import sqlite3, os, sys

DB_PATH = os.path.join(
    os.path.dirname(os.path.dirname(__file__)),
    'db_builder', 'arknights.db'
)


def main():
    before = os.path.getsize(DB_PATH)
    conn = sqlite3.connect(DB_PATH)

    # 检查碎片率
    freelist = conn.execute('PRAGMA freelist_count').fetchone()[0]
    page_count = conn.execute('PRAGMA page_count').fetchone()[0]
    page_size = conn.execute('PRAGMA page_size').fetchone()[0]
    free_pct = freelist / page_count * 100 if page_count else 0

    print(f'VACUUM 前: {before/1024/1024:.1f} MB')
    print(f'页面: {page_count}, 空闲页: {freelist} ({free_pct:.1f}%)')

    if free_pct < 5:
        print('碎片率不足 5%，不建议执行 VACUUM')
        conn.close()
        return

    conn.execute('VACUUM')
    conn.close()

    after = os.path.getsize(DB_PATH)
    saved = before - after
    print(f'VACUUM 后: {after/1024/1024:.1f} MB (回收 {saved/1024/1024:.1f} MB)')


if __name__ == '__main__':
    main()
