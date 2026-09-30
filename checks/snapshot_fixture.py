"""Create a fresh synthetic packed archive for both search browser checks."""
from contextlib import closing
from pathlib import Path
import sqlite3
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from checks.profile_fixture import create
from search_snapshot import build_snapshot


def main(directory):
    directory = Path(directory).resolve()
    source = directory / 'source'
    create(source)
    with closing(sqlite3.connect(source / 'searchcord.db')) as db, db:
        db.execute("UPDATE messages SET content = 'hello hi ' || content")
    packed = directory / 'packed' / 'searchcord.db'
    report = build_snapshot(source / 'searchcord.db', packed)
    print(f"Verified synthetic snapshot: {report['messages']} messages.")


if __name__ == '__main__':
    main(sys.argv[1])
