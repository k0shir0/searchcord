import os
import json
from flask import Flask, render_template_string, request, jsonify

app = Flask(__name__)

# Load all JSON files from current directory
DATA = []
FILES = [f for f in os.listdir('.') if f.endswith('.json')]
for f in FILES:
    with open(f, 'r', encoding='utf-8') as file:
        try:
            messages = json.load(file)
            for msg in messages:
                msg['source'] = f.replace('.json', '')
            DATA.extend(messages)
        except Exception:
            pass

# HTML Template with modern UI
HTML = """
<!DOCTYPE html>
<html>
<head>
    <title>Discord Scraper Viewer</title>
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
            max-width: 900px;
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
            margin: 12px 0;
            border-radius: 12px;
            box-shadow: 0px 2px 6px rgba(0,0,0,0.4);
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
        .source {
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
    </style>
</head>
<body>
    <header>📂 Discord Scraper Viewer</header>
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
                html += '<div class="source">From: ' + msg.source + '</div>';
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
    query = request.args.get('query', '').lower()
    if not query:
        return jsonify(DATA[:200])  # default show some data
    results = [msg for msg in DATA if query in msg.get('username','').lower() or query in msg.get('message','').lower()]
    return jsonify(results)

if __name__ == "__main__":
    app.run(debug=True)
