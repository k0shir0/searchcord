SearchCord
==========

SearchCord is a two-part tool for collecting and searching through Discord messages.  
It includes:

- scrape.py → A scraper that collects messages from servers or DMs into .json files.  
- search.py → A Flask-based web app that lets you search across all scraped datasets with a clean, user-friendly interface.  

------------------------------------------------------------
Features
------------------------------------------------------------
- Collect messages into per-channel JSON datasets.
- Automatically saves images and message timestamps.
- Simple, modern web interface with blue/pink styling.
- Search by username or message text.
- Results displayed in chat-style message cards.
- Load more button to paginate results in batches of 50.
- Supports multiple datasets from multiple servers.

------------------------------------------------------------
Project Structure
------------------------------------------------------------
SearchCord/
│
├── scrape.py      # Scraper script
├── search.py      # Web interface (Flask app)
├── data.json      # Example JSON dataset (created by scraper)
└── README.txt

------------------------------------------------------------
Installation
------------------------------------------------------------
1. Make sure you have Python 3.9+ installed.
2. Install required dependencies:

   pip install flask requests

3. (Optional) Use a virtual environment to keep things clean:

   python3 -m venv venv
   source venv/bin/activate

------------------------------------------------------------
Usage
------------------------------------------------------------

1. Scraping Messages
--------------------
Run the scraper to collect messages into JSON files.

   python3 scrape.py

- Choose whether to scrape servers or DMs.
- Select a channel or conversation.
- Provide a timeframe (e.g. 5h, 2d, 1m).
- A JSON file will be created with the name format:

   servername_channelname.json

2. Searching Messages
---------------------
Once you have scraped data, start the web interface:

   python3 search.py

Then open your browser and go to:

   http://127.0.0.1:5000

------------------------------------------------------------
Web Interface
------------------------------------------------------------
- Enter a username or part of a message in the search bar.
- Up to 50 messages are shown at first.
- Click "Display More" to load additional results.
- Each message card shows:
  * Username
  * Message text
  * Image (if attached)
  * Source file (server/channel it came from)

------------------------------------------------------------
Design
------------------------------------------------------------
The interface is styled with:
- Blue + Pink gradient header
- Dark theme background
- Rounded, shadowed message cards
- Embedded images styled with borders and rounded corners

------------------------------------------------------------
Disclaimer
------------------------------------------------------------
This project is for educational and personal archival purposes only.  
Do not use it in violation of Discord’s Terms of Service.  
You are responsible for how you use this software.
