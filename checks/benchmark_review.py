"""Reproduce read latency on synthetic data, never a personal Discord archive.

python checks/benchmark_review.py /new/fixture --rows 9000000 --report /new/results.json
Bulk fixture construction bypasses ingestion triggers, then rebuilds the same
indexes and counters. Build time is not a collector-throughput benchmark.
"""
import argparse
import asyncio
import json
import platform
import sqlite3
import statistics
import sys
import time
from contextlib import closing
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import app
from profile_store import profile_servers
from storage import init_database


def create(path, rows):
    if path.exists():
        with sqlite3.connect(path) as db:
            if db.execute('SELECT COUNT(*) FROM messages').fetchone()[0] != rows:
                raise ValueError('Existing fixture has a different row count; use a new directory.')
        return
    path.parent.mkdir(parents=True, exist_ok=True)
    init_database(str(path))
    with closing(sqlite3.connect(path)) as db, db:
        triggers = db.execute("SELECT name,sql FROM sqlite_master WHERE type='trigger'").fetchall()
        for name, _ in triggers:
            db.execute('DROP TRIGGER "' + name + '"')
        db.executemany('INSERT INTO guilds VALUES (?,?)', [(str(100+i), f'Example server {i}') for i in range(20)])
        db.executemany('INSERT INTO channels VALUES (?,?,?)', [(str(200+i),str(100+i%20),f'channel-{i}') for i in range(100)])
        db.executemany('INSERT INTO authors(id,name) VALUES (?,?)', [(str(i),f'Example author {i}') for i in range(50,10050)])
        db.executemany('INSERT INTO profiles(user_id,username,fetched_at) VALUES (?,?,?)', [(str(i),f'user{i}','2026-10-02') for i in range(50,10050)])
        for start in range(0, rows, 100000):
            batch = ((1000000000000000000+i*4194304, 200+i%100,100+i%20,
                      50 if i%4==0 else 50+i%10000,
                      ('release notes ' if i%10007==0 else 'hello ' if i%5==0 else '')+
                      f'Synthetic archive message {i} with ordinary searchable text.')
                     for i in range(start,min(start+100000,rows)))
            db.executemany('INSERT INTO messages(id,channel_id,guild_id,author_id,content) VALUES (?,?,?,?,?)',batch)
            print(f'Inserted {min(start+100000,rows):,}',flush=True)
        print('Rebuilding FTS and counters',flush=True)
        db.execute("INSERT INTO messages_fts(messages_fts) VALUES ('rebuild')")
        db.execute("INSERT INTO messages_fts(messages_fts) VALUES ('optimize')")
        db.execute("INSERT INTO stats_counts VALUES ('total','',?)",(rows,))
        for kind,expr in [('guild','guild_id'),('channel','channel_id'),('author','author_id'),
                          ('day',"date(((id >> 22)+1420070400000)/1000.0,'unixepoch')"),
                          ('hour',"strftime('%H',((id >> 22)+1420070400000)/1000.0,'unixepoch')")]:
            db.execute(f'INSERT INTO stats_counts SELECT ?,CAST({expr} AS TEXT),COUNT(*) FROM messages GROUP BY {expr}',(kind,))
        for _,sql in triggers:
            db.execute(sql)
        db.commit()
        db.execute('PRAGMA wal_checkpoint(TRUNCATE)')
        db.execute('ANALYZE')
        db.commit()
        db.execute('PRAGMA wal_checkpoint(TRUNCATE)')


def measure(fn, rounds):
    timings=[]
    for _ in range(rounds):
        start=time.perf_counter();result=fn();timings.append((time.perf_counter()-start)*1000)
    return {'median_ms':round(statistics.median(timings),3),'max_ms':round(max(timings),3),'samples_ms':[round(v,3) for v in timings]},result


def run(path, rounds):
    app.DB_PATH=str(path)
    report={'python':platform.python_version(),'sqlite':sqlite3.sqlite_version,'rounds':rounds,
            'cache':'Fresh SQLite connections; OS cache not flushed; sequential in-process calls, not HTTP.',
            'database_bytes':path.stat().st_size,'queries':[]}
    with sqlite3.connect(path) as db:
        report['messages']=db.execute('SELECT COUNT(*) FROM messages').fetchone()[0]
        report['profile_index_bytes']=db.execute("SELECT SUM(pgsize) FROM dbstat WHERE name='idx_profiles_username'").fetchone()[0]
    for name,filters in [('browse',{}),('common',{'q':'hello'}),('phrase',{'q':'release notes'}),
                         ('absent',{'q':'zzzzunlikelymatch'}),('author',{'author_id':'50'}),
                         ('combined',{'q':'hello','author_id':'50'})]:
        old,a=measure(lambda filters=filters:asyncio.run(app.search(**filters,limit=50)),rounds)
        new,b=measure(lambda filters=filters:asyncio.run(app.search(**filters,limit=50,cursor=True)),rounds)
        assert [m['id'] for m in a['messages']]==[m['id'] for m in b['messages']]
        report['queries'].append({'class':name,'legacy':old,'cursor':new,'same_ids':True})
        print(name,old['median_ms'],new['median_ms'],flush=True)
    # The original profile query from reviewed base 6ee9255.
    def legacy_profile(db, uid):
        from profile_store import search_label
        db.row_factory=sqlite3.Row
        db.create_function('search_label',1,search_label,deterministic=True)
        rows=db.execute("""SELECT CAST(m.guild_id AS TEXT) AS id,
            COALESCE(g.name, 'Unknown server') AS name, COUNT(*) AS messages,
            CAST(MAX(m.id) AS TEXT) AS latest_id
            FROM messages m LEFT JOIN guilds g ON g.id=CAST(m.guild_id AS TEXT)
            WHERE m.author_id=? AND m.guild_id IS NOT NULL
              AND (search_label(g.name) LIKE ? ESCAPE '!' OR CAST(m.guild_id AS TEXT)=?)
            GROUP BY m.guild_id ORDER BY messages DESC,m.guild_id LIMIT ? OFFSET ?""",
            (int(uid),'%%','',31,0)).fetchall()
        return {'servers':[dict(row) for row in rows[:30]],'has_more':len(rows)>30,'offset':0}
    def profiles(fn):
        with closing(sqlite3.connect(path.as_uri()+'?mode=ro',uri=True)) as db:
            return fn(db,'50')
    old,a=measure(lambda:profiles(legacy_profile),rounds)
    new,b=measure(lambda:profiles(profile_servers),rounds)
    assert a==b
    report['profiles']={'legacy':old,'group_first':new,'same_results':True}
    return report


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory',type=Path)
    parser.add_argument('--rows',type=int,default=1000000)
    parser.add_argument('--rounds',type=int,default=5)
    parser.add_argument('--report',type=Path,required=True)
    args=parser.parse_args()
    if args.rows<1 or not 1<=args.rounds<=20 or args.report.exists():
        parser.error('Use positive rows, 1-20 rounds and a new report path.')
    path=args.directory.resolve()/'searchcord.db'
    create(path,args.rows)
    report=run(path,args.rounds)
    args.report.write_text(json.dumps(report,indent=2)+'\n')
    print(json.dumps(report,indent=2))
