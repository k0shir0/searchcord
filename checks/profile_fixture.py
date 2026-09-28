"""Create a new, entirely synthetic archive for the profile browser checks."""
from contextlib import closing
import json
from pathlib import Path
import sqlite3
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from storage import init_database


def create(directory):
    directory = Path(directory).resolve()
    directory.mkdir(parents=True, exist_ok=True)
    path = directory / 'searchcord.db'
    if path.exists():
        raise SystemExit('Refusing to overwrite an existing archive.')
    init_database(str(path))
    with closing(sqlite3.connect(path)) as db, db:
        db.executemany('INSERT INTO authors(id,name) VALUES (?,?)',
                       [('50','Alice Example'),('60','Bob Example')])
        for i in range(45):
            guild, channel = 100+i, 200+i
            db.execute('INSERT INTO guilds VALUES (?,?)', (str(guild),'Garden Club' if i==0 else f'Example server {i:02d}'))
            db.execute('INSERT INTO channels VALUES (?,?,?)', (str(channel),str(guild),'📢┃general-chat' if i==0 else f'channel-{i:02d}'))
            for j in range(46-i):
                db.execute('INSERT INTO messages(id,channel_id,guild_id,author_id,content) VALUES (?,?,?,?,?)',
                           (1000000000000000000+i*100+j,channel,guild,50,f'Synthetic message {i}-{j}'))
        db.execute('INSERT INTO messages(id,channel_id,guild_id,author_id,content) VALUES (?,?,?,?,?)',
                   (1000000000000010000,200,100,60,'Synthetic message from Bob'))
        payload = {'user':{'id':'50','username':'alice.example','global_name':'Alice Example'},
                   'user_profile':{'bio':'A synthetic profile for local browser checks.','pronouns':'she/her'},
                   'badges':[{'id':'example','description':'Example badge'}],
                   'connected_accounts':[{'type':'github','name':'alice-example','verified':True},
                                         {'type':'youtube','name':'Alice Example'}]}
        db.execute('INSERT INTO profiles(user_id,username,global_name,fetched_at) VALUES (?,?,?,?)',
                   ('50','alice.example','Alice Example','2026-09-28T00:00:00+00:00'))
        db.execute('INSERT INTO profile_details VALUES (?,?,?)',('50',json.dumps(payload),'2026-09-28T00:00:00+00:00'))
    print('Created synthetic profile archive: 1081 messages, 45 servers, 2 authors.')


if __name__ == '__main__':
    create(sys.argv[1])
