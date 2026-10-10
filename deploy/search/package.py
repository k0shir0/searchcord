"""Build a search-only directory without collector code, credentials or data."""
import argparse
import shutil
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
FILES = (
    '.dockerignore',
    'search_app.py', 'search_snapshot.py', 'profile_store.py', 'media_store.py', 'cli.py', 'LICENSE',
    'deploy/search/requirements.txt', 'deploy/search/Dockerfile',
    'deploy/search/Dockerfile.dockerignore', 'deploy/search/README.md',
    'static/search/index.html', 'static/search/search.css', 'static/search/search.js',
    'static/search/privacy.html', 'static/search/privacy.css',
    'static/bauhaus.css', 'static/profile-view.css', 'static/profile-view.js',
)


def build(destination):
    destination = Path(destination).resolve()
    destination.mkdir(parents=True, exist_ok=False)
    try:
        for name in FILES:
            target = destination / name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(ROOT / name, target)
        (destination / 'data').mkdir()
    except BaseException:
        shutil.rmtree(destination)
        raise
    return destination


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('destination', type=Path)
    args = parser.parse_args()
    print(build(args.destination))
