import requests
import json
import time
from datetime import datetime, timedelta, timezone
import re

# --- User input ---
TOKEN = input("Enter your Discord user token: ")
HEADERS = {
    "Authorization": TOKEN,
    "Content-Type": "application/json"
}

DISCORD_EPOCH = 1420070400000  # Discord epoch in milliseconds

def datetime_to_snowflake(dt):
    timestamp_ms = int(dt.timestamp() * 1000)
    snowflake = (timestamp_ms - DISCORD_EPOCH) << 22
    return snowflake

def list_dms():
    response = requests.get("https://discord.com/api/v10/users/@me/channels", headers=HEADERS)
    dms = response.json()
    for i, dm in enumerate(dms):
        recipient = dm['recipients'][0]['username'] if dm['type'] == 1 else "Group DM"
        print(f"{i}: {recipient}")
    return dms

def list_servers():
    response = requests.get("https://discord.com/api/v10/users/@me/guilds", headers=HEADERS)
    guilds = response.json()
    for i, guild in enumerate(guilds):
        print(f"{i}: {guild['name']}")
    return guilds

def list_channels(guild_id):
    response = requests.get(f"https://discord.com/api/v10/guilds/{guild_id}/channels", headers=HEADERS)
    channels = response.json()
    for i, channel in enumerate(channels):
        print(f"{i}: {channel['name']} ({channel['id']})")
    return channels

def parse_time_frame(time_frame):
    now = datetime.now(timezone.utc)
    num = int(time_frame[:-1])
    unit = time_frame[-1].lower()
    
    if unit == 'h':
        delta = timedelta(hours=num)
    elif unit == 'd':
        delta = timedelta(days=num)
    elif unit == 'm':
        delta = timedelta(days=num*30)
    else:
        print("Invalid time format. Using 1 day as default.")
        delta = timedelta(days=1)
    
    return now - delta

def sanitize_filename(name):
    """Remove emojis and keep only letters, numbers, underscores, and dashes."""
    return re.sub(r'[^A-Za-z0-9_\-]', '_', name)

def scrape_messages(channel_id, time_frame, custom_name):
    url = f"https://discord.com/api/v10/channels/{channel_id}/messages"
    after_datetime = parse_time_frame(time_frame)
    after_snowflake = str(datetime_to_snowflake(after_datetime))
    messages_data = []
    seen_ids = set()  # Track message IDs to avoid duplicates

    params = {"after": after_snowflake, "limit": 100}

    while True:
        response = requests.get(url, headers=HEADERS, params=params)

        if response.status_code == 429:
            retry_after = response.json().get("retry_after", 1)
            print(f"Rate limited. Sleeping for {retry_after} seconds...")
            time.sleep(retry_after)
            continue

        if response.status_code != 200:
            print(f"Error: {response.status_code} - {response.text}")
            break

        messages = response.json()
        if not messages:
            break

        for msg in messages:
            if msg['id'] in seen_ids:  # Skip duplicates
                continue
            seen_ids.add(msg['id'])
            entry = {
                "username": msg['author']['username'],
                "message": msg.get('content', ''),
                "img": msg['attachments'][0]['url'] if msg.get('attachments') else None,
                "time_sent": msg['timestamp']
            }
            messages_data.append(entry)

        params['after'] = messages[-1]['id']
        time.sleep(0.1)

    # Sort messages chronologically
    messages_data.sort(key=lambda x: x['time_sent'])

    filename = sanitize_filename(custom_name) + ".json"
    with open(filename, 'w', encoding='utf-8') as f:
        json.dump(messages_data, f, indent=4)

    print(f"Scraped {len(messages_data)} messages. Saved to {filename}")

# --- Main program ---
print("Choose an option:")
print("1: DMs")
print("2: Servers")
choice = input("> ")

if choice == "1":
    dms = list_dms()
    dm_index = int(input("Choose a DM to scrape: "))
    channel_id = dms[dm_index]['id']
    custom_name = input("Enter custom filename (without extension): ")
elif choice == "2":
    guilds = list_servers()
    guild_index = int(input("Choose a server: "))
    channels = list_channels(guilds[guild_index]['id'])
    channel_index = int(input("Choose a channel to scrape: "))
    channel_id = channels[channel_index]['id']
    custom_name = f"{guilds[guild_index]['name']}_{channels[channel_index]['name']}"
else:
    print("Invalid choice")
    exit()

time_frame = input("Enter time frame (e.g., 1-99h, 1-99d, 1-99m): ")
scrape_messages(channel_id, time_frame, custom_name)
