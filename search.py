import os
import json
import re
from flask import Flask, render_template_string, request, jsonify

app = Flask(__name__)

# Load all JSON files from current directory
DATA = []
FILES = [f for f in os.listdir('.') if f.endswith('.json')]
seen_ids = set()

for f in FILES:
    with open(f, 'r', encoding='utf-8') as file:
        try:
            messages = json.load(file)
            for msg in messages:
                msg_id = msg.get('id')
                if msg_id and msg_id not in seen_ids:
                    seen_ids.add(msg_id)
                    msg['source'] = f.replace('.json', '')
                    DATA.append(msg)
        except Exception:
            pass

# HTML Template with animations
HTML = """
<!DOCTYPE html>
<html>
<head>
    <title>Searchcord Viewer</title>
    <style>
        body {
            font-family: 'Segoe UI', Tahoma, sans-serif;
            background: linear-gradient(135deg, #1e1e2f, #2a003f);
            color: #f5f5f5;
            margin: 0;
            padding: 0;
        }
        header {
            background: linear-gradient(90deg, #0077ff, #ff3cac);
            padding: 20px;
            text-align: center;
            font-size: 1.8em;
            font-weight: bold;
            color: white;
            letter-spacing: 1px;
        }
        main {
            max-width: 950px;
            margin: 20px auto;
            padding: 15px;
        }
        .search-bar {
            display: flex;
            justify-content: center;
            margin-bottom: 20px;
        }
        .search-bar input {
            width: 70%;
            padding: 10px;
            border: none;
            border-radius: 8px 0 0 8px;
            outline: none;
            font-size: 1em;
        }
        .search-bar button {
            background: #0077ff;
            border: none;
            padding: 10px 20px;
            color: white;
            font-size: 1em;
            border-radius: 0 8px 8px 0;
            cursor: pointer;
            transition: 0.3s;
        }
        .search-bar button:hover {
            background: #005bd1;
        }
        .message {
            background: #2d2d44;
            padding: 15px;
            margin: 15px 0;
            border-radius: 12px;
            box-shadow: 0px 2px 6px rgba(0,0,0,0.4);
            opacity: 0;
            transform: translateY(20px);
            animation: fadeInUp 0.5s forwards;
        }
        .username {
            font-weight: bold;
            color: #ff3cac;
            font-size: 1.1em;
        }
        .text {
            margin-top: 6px;
            font-size: 1em;
            color: #e0e0e0;
        }
        .img {
            max-width: 250px;
            max-height: 250px;
            margin-top: 10px;
            border-radius: 10px;
            border: 2px solid #0077ff;
        }
        .meta {
            font-size: 0.8em;
            margin-top: 8px;
            color: #9d9d9d;
        }
        #loadMore {
            display: block;
            margin: 25px auto;
            padding: 10px 25px;
            background: #ff3cac;
            color: white;
            font-size: 1em;
            border: none;
            border-radius: 8px;
            cursor: pointer;
            transition: 0.3s;
        }
        #loadMore:hover {
            background: #d20072;
        }
        @keyframes fadeInUp {
            from { opacity: 0; transform: translateY(20px); }
            to { opacity: 1; transform: translateY(0); }
        }
    </style>
</head>
<body>
    <header>🔍 Searchcord Viewer</header>
    <main>
        <div class="search-bar">
            <input type="text" id="search" placeholder="Search username or message...">
            <button onclick="search()">Search</button>
        </div>
        <div id="results"></div>
        <button id="loadMore" style="display:none;" onclick="loadMore()">Display More</button>
    </main>

    <script>
        let data = [];
        let currentIndex = 0;
        const pageSize = 50;

        function search() {
            const query = document.getElementById('search').value.toLowerCase();
            fetch('/search?query=' + encodeURIComponent(query))
                .then(res => res.json())
                .then(resData => {
                    data = resData;
                    currentIndex = 0;
                    document.getElementById('results').innerHTML = '';
                    loadMore();
                });
        }

        function loadMore() {
            const container = document.getElementById('results');
            const nextIndex = Math.min(currentIndex + pageSize, data.length);
            for (let i = currentIndex; i < nextIndex; i++) {
                const msg = data[i];
                let html = '<div class="message">';
                html += '<div class="username">' + msg.username + '</div>';
                html += '<div class="text">' + msg.message + '</div>';
                if (msg.img) html += '<img class="img" src="' + msg.img + '">';
                html += '<div class="meta">From: ' + msg.source + '</div>';
                html += '<div class="meta">Sent: ' + msg.time_sent + '</div>';
                html += '<div class="meta">Message ID: ' + msg.id + '</div>';
                html += '</div>';
                container.innerHTML += html;
            }
            currentIndex = nextIndex;
            document.getElementById('loadMore').style.display = (currentIndex < data.length) ? 'block' : 'none';
        }
    </script>
</body>
</html>
"""

@app.route('/')
def index():
    return render_template_string(HTML)

@app.route('/search')
def search():
    query = request.args.get('query', '').lower().strip()
    if not query:
        return jsonify(DATA[:200])  # default show some data

    results = DATA
    from_user = None
    contains_text = None
    has_image = None

    # Parse from:(user)
    match_from = re.search(r'from:\((.*?)\)', query)
    if match_from:
        from_user = match_from.group(1).lower()
        query = re.sub(r'from:\(.*?\)', '', query).strip()

    # Parse contains:(text)
    match_contains = re.search(r'contains:\((.*?)\)', query)
    if match_contains:
        contains_text = match_contains.group(1).lower()
        query = re.sub(r'contains:\(.*?\)', '', query).strip()

    # Parse has:image_true
    if 'has:image_true' in query:
        has_image = True
        query = query.replace('has:image_true', '').strip()

    # Filter messages and remove duplicates
    filtered = []
    seen_ids_local = set()
    for msg in results:
        msg_id = msg.get('id')
        if msg_id in seen_ids_local:
            continue
        seen_ids_local.add(msg_id)

        if from_user and from_user not in msg.get('username', '').lower():
            continue
        if contains_text and contains_text not in msg.get('message', '').lower():
            continue
        if has_image and not msg.get('img'):
            continue
        if query and query not in msg.get('message', '').lower() and query not in msg.get('username','').lower():
            continue

        filtered.append(msg)

    return jsonify(filtered[:200])


if __name__ == "__main__":
    app.run(debug=True)
