import requests
import json
import time
import threading
from datetime import datetime, timedelta, timezone
import re
from queue import Queue

# --- User input ---
TOKEN = input("Enter your Discord user token: ")
HEADERS = {
    "Authorization": TOKEN,
    "Content-Type": "application/json"
}

DISCORD_EPOCH = 1420070400000  # Discord epoch in ms
SAVE_INTERVAL = 200  # save every N messages

# --- Utility Functions ---
def datetime_to_snowflake(dt):
    timestamp_ms = int(dt.timestamp() * 1000)
    return str((timestamp_ms - DISCORD_EPOCH) << 22)

def parse_time_frame(time_frame):
    now = datetime.now(timezone.utc)
    num = int(time_frame[:-1])
    unit = time_frame[-1].lower()
    if unit == 'h':
        delta = timedelta(hours=num)
    elif unit == 'd':
        delta = timedelta(days=num)
    elif unit == 'm':
        delta = timedelta(days=num * 30)
    else:
        delta = timedelta(days=1)
    return now - delta

def sanitize_filename(name):
    return re.sub(r'[^A-Za-z0-9_\-]', '_', name)

def save_message_to_json(filename, msg):
    try:
        data = []
        try:
            with open(filename, "r", encoding="utf-8") as f:
                data = json.load(f)
        except FileNotFoundError:
            pass
        # Prevent duplicates
        if msg["id"] not in [m["id"] for m in data]:
            data.append({
                "id": msg["id"],
                "username": msg["author"]["username"],
                "message": msg.get("content", ""),
                "img": msg["attachments"][0]["url"] if msg.get("attachments") else None,
                "time_sent": msg["timestamp"]
            })
            with open(filename, "w", encoding="utf-8") as f:
                json.dump(data, f, indent=4)
    except Exception as e:
        print(f"[-] Error saving message: {e}")

# --- Server & DM Listing ---
def list_dms():
    r = requests.get("https://discord.com/api/v10/users/@me/channels", headers=HEADERS)
    dms = r.json()
    for i, dm in enumerate(dms):
        recipient = dm['recipients'][0]['username'] if dm['type'] == 1 else "Group DM"
        print(f"{i}: {recipient}")
    return dms

def list_servers():
    r = requests.get("https://discord.com/api/v10/users/@me/guilds", headers=HEADERS)
    guilds = r.json()
    for i, g in enumerate(guilds):
        print(f"{i}: {g['name']}")
    return guilds

def list_channels(guild_id):
    r = requests.get(f"https://discord.com/api/v10/guilds/{guild_id}/channels", headers=HEADERS)
    chans = r.json()
    for i, c in enumerate(chans):
        print(f"{i}: {c['name']} ({c['id']})")
    return chans

# --- Historical Scrape ---
def fetch_messages(channel_id, direction, start_id, out_queue):
    url = f"https://discord.com/api/v10/channels/{channel_id}/messages"
    params = {"limit": 100}
    params[direction] = start_id

    while True:
        response = requests.get(url, headers=HEADERS, params=params)

        if response.status_code == 429:
            retry_after = response.json().get("retry_after", 1)
            print(f"Rate limited ({direction}). Sleeping {retry_after}s...")
            time.sleep(retry_after)
            continue

        if response.status_code != 200:
            print(f"Error {direction}: {response.status_code} - {response.text}")
            break

        messages = response.json()
        if not messages:
            break

        # always ordered newest -> oldest, so adjust pagination
        if direction == "after":
            params["after"] = messages[-1]["id"]
        else:
            params["before"] = messages[-1]["id"]

        for msg in messages:
            out_queue.put({
                "id": msg["id"],
                "username": msg["author"]["username"],
                "message": msg.get("content", ""),
                "img": msg["attachments"][0]["url"] if msg.get("attachments") else None,
                "time_sent": msg["timestamp"]
            })

def scrape_messages(channel_id, time_frame, custom_name):
    after_datetime = parse_time_frame(time_frame)
    after_snowflake = datetime_to_snowflake(after_datetime)

    out_queue = Queue()
    threads = []

    # thread 1: forward from cutoff
    t1 = threading.Thread(target=fetch_messages, args=(channel_id, "after", after_snowflake, out_queue))
    threads.append(t1)

    # thread 2: backward from now
    t2 = threading.Thread(target=fetch_messages, args=(channel_id, "before", "999999999999999999", out_queue))
    threads.append(t2)

    for t in threads:
        t.start()

    messages_data = {}
    saved_count = 0

    while any(t.is_alive() for t in threads) or not out_queue.empty():
        while not out_queue.empty():
            msg = out_queue.get()
            if msg["id"] not in messages_data:  # dedup by ID
                messages_data[msg["id"]] = msg

        if len(messages_data) - saved_count >= SAVE_INTERVAL:
            filename = sanitize_filename(custom_name) + ".json"
            with open(filename, "w", encoding="utf-8") as f:
                json.dump(sorted(messages_data.values(), key=lambda x: x["time_sent"]), f, indent=4)
            saved_count = len(messages_data)
            print(f"Progress: {saved_count} messages saved...")

        time.sleep(0.2)

    for t in threads:
        t.join()

    # final save
    filename = sanitize_filename(custom_name) + ".json"
    with open(filename, "w", encoding="utf-8") as f:
        json.dump(sorted(messages_data.values(), key=lambda x: x["time_sent"]), f, indent=4)

    print(f"Finished: {len(messages_data)} messages scraped. Saved to {filename}")

# --- Live Monitor ---
def live_monitor(channel_id):
    filename = f"channel_{channel_id}_live.json"
    last_message_id = None
    print("\n[+] Live monitoring started. Press CTRL+C to stop.\n")
    try:
        while True:
            url = f"https://discord.com/api/v10/channels/{channel_id}/messages?limit=50"
            if last_message_id:
                url += f"&after={last_message_id}"

            try:
                r = requests.get(url, headers=HEADERS)
                if r.status_code != 200:
                    print(f"[-] Error fetching messages: {r.status_code} {r.text}")
                    time.sleep(2)
                    continue
                messages = r.json()
            except Exception as e:
                print(f"[-] Exception: {e}")
                time.sleep(2)
                continue

            messages = sorted(messages, key=lambda x: int(x["id"]))  # oldest first

            for msg in messages:
                author = msg["author"]["username"]
                content = msg.get("content", "")
                attachments = [a["url"] for a in msg.get("attachments", [])]
                print(f"{author}: {content}")
                if attachments:
                    for a in attachments:
                        print(f"[Attachment] {a}")
                save_message_to_json(filename, msg)
                last_message_id = msg["id"]  # update to latest message

            time.sleep(2)

    except KeyboardInterrupt:
        print("\n[!] Live monitoring stopped. Data saved.")

# --- Main Program ---
print("Choose an option:")
print("1: DMs")
print("2: Servers")
choice = input("> ")

if choice == "1":
    dms = list_dms()
    dm_index = int(input("Choose a DM: "))
    channel_id = dms[dm_index]['id']
    custom_name = input("Custom filename (no extension): ")
    time_frame = input("Enter time frame (e.g., 1-99h, 1-99d, 1-99m): ")
    scrape_messages(channel_id, time_frame, custom_name)

elif choice == "2":
    guilds = list_servers()
    guild_index = int(input("Choose a server: "))
    channels = list_channels(guilds[guild_index]['id'])
    channel_index = int(input("Choose a channel: "))
    channel_id = channels[channel_index]['id']

    print("Choose mode:")
    print("1: Live-Monitor")
    print("2: ScrapeBack")
    mode = input("> ")

    if mode == "1":
        live_monitor(channel_id)
    elif mode == "2":
        custom_name = f"{guilds[guild_index]['name']}_{channels[channel_index]['name']}"
        time_frame = input("Enter time frame (e.g., 1-99h, 1-99d, 1-99m): ")
        scrape_messages(channel_id, time_frame, custom_name)
    else:
        print("Invalid choice")
        exit()
else:
    print("Invalid choice")
    exit()
