# Searchcord

A local, self-hosted tool for archiving and searching Discord messages. Runs
entirely on your machine as a single FastAPI app with a browser UI — scrape
channels into SQLite, then search, filter, and chart what you collected.

> **Read the [wrongful use warning](LICENSE) before running this.** This tool
> archives other people's private conversations, automating a user account
> against Discord's API violates their Terms of Service, and the risk is
> entirely yours. Use it on your own history or with the consent of the people
> involved.

---

## Features

- **Browse** your servers and channels, or your DMs and group DMs.
- **Queue and scrape** any number of channels at once, with an optional
  per-channel message cap, live progress, and a stop button.
- **Live monitor** channels and watch new messages stream in as they arrive.
- **Search** everything you have collected, filtered by server, channel,
  author, and date range, with match highlighting and pagination.
- **Stats** — totals, top senders, messages per server, activity over the
  last 30 days, and a by-hour histogram.
- **ChatML export** — turn a conversation into a `.jsonl` file in the
  OpenAI/ChatML message format.

Everything is stored in one SQLite file at `data/searchcord.db`. Nothing is
sent anywhere except to Discord's own API.

---

## Requirements

- Python 3.9 or newer
- A Discord token

## Installation

```bash
git clone https://github.com/k0shir0/searchcord.git
cd searchcord
pip install -r requirements.txt
```

## Running

```bash
python app.py
```

The app starts on <http://127.0.0.1:8000> and opens your browser. On Windows
you can double-click `start.bat` instead.

It binds to loopback only. There is **no authentication** — anyone who can
reach the port gets your token and your entire archive — so only change the
host if you understand that:

```bash
SEARCHCORD_HOST=0.0.0.0 SEARCHCORD_PORT=8000 python app.py
```

## First run

Open the hamburger menu, paste your token, and hit **Save & Verify**. Once it
reports a connected username your servers will load in the **browse** tab.

To scrape: click channels to add them to the queue, optionally set a
"msgs back" limit, then **start scraping**. Leave the limit blank to pull the
full history.

---

## Project structure

```
searchcord/
├── app.py             # FastAPI backend — API, scraper, live poller, export
├── requirements.txt
├── start.bat          # Windows launcher
├── static/
│   ├── index.html
│   ├── app.js         # Frontend logic
│   └── style.css
└── data/              # Created at runtime — gitignored, never commit
    └── searchcord.db
```

---

## Data & privacy

`data/searchcord.db` contains **your Discord token in plaintext** in the
`settings` table, alongside every message you have scraped. The `data/`
directory is gitignored for that reason. Do not commit it, do not share it,
and delete it when you are done. **Clear All Data** in the settings panel
wipes the messages table; deleting the file removes everything including the
token.

If you ever push this database anywhere by accident, treat your token as
compromised and reset it immediately by changing your Discord password.

## Notes

- Uses your user token against Discord's HTTP API. This is against Discord's
  Terms of Service and can get your account terminated. You accept that risk
  by running it.
- Requests are rate-limit aware and back off on HTTP 429, with a retry cap.
- Scraping pages backwards 100 messages at a time with a short delay between
  batches; a full server takes a while.

## License

MIT, with a wrongful use warning — see [LICENSE](LICENSE).
