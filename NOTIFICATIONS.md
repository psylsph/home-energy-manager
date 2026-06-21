# Notification Setup Guide

Home Energy Manager can send push notifications when your inverter detects
issues — battery temperature warnings, grid outages, solar clipping, and more.
Three notification methods are supported: **Telegram**, **ntfy**, and
**Pushover**. They work independently, so you can set up one, two, or all
three.

---

## Telegram

Telegram is a free messaging app. You create a simple bot that forwards alerts
to your phone.

### Step 1: Create a bot

1. Open Telegram and search for **[@BotFather](https://t.me/botfather)** — this
   is the official bot that creates other bots.
2. Start a chat and send: `/newbot`
3. BotFather will ask for a **name** — this is the display name, e.g.
   `Home Energy Manager`.
4. It will then ask for a **username** — this must end in `bot`, e.g.
   `my_energy_bot`.
5. Once created, BotFather will give you a **bot token**. It looks like:

   ```
   123456789:ABCdefGHIjklmNOPqrstUVwxyz123456789
   ```

   **Copy this token** — you will paste it into the app later.

### Step 2: Get your Chat ID

1. Open Telegram and search for your bot's username (e.g. `@my_energy_bot`).
2. Click **Start** or send any message like *hello*.
3. Now search for **[@userinfobot](https://t.me/userinfobot)** and send it any
   message — it will reply with your **Chat ID** (a number like `123456789`).

   **Alternative:** Open this URL in your browser (replace `YOUR_TOKEN`):

   ```
   https://api.telegram.org/botYOUR_TOKEN/getUpdates
   ```

   Look for `"chat":{"id":123456789}` in the response.

### Step 3: Enter them in the app

1. Open the Home Energy Manager dashboard.
2. Go to **Settings** → scroll to **Notifications**.
3. Toggle **Enable Alerts** on.
4. Paste the bot token into **Bot Token**.
5. Paste the Chat ID into **Chat ID**.
6. Adjust any temperature or SOC thresholds you care about.
7. Click **Save** — a test message should arrive in Telegram shortly.

### Troubleshooting

- **"Chat not found"** — Make sure you have messaged your bot at least once
  before testing (Step 2). Bots can't initiate conversations.
- **Token is wrong** — Double-check the token from BotFather. It should be a
  long string with letters, numbers, and a colon in the middle.
- **No notification for a specific alert** — Check that the relevant toggle
  is enabled under **Alert Triggers** (e.g. Grid Offline, Solar Clipping).

---

## ntfy

[ntfy](https://ntfy.sh) is a simpler alternative — no accounts, no tokens,
no bots. You subscribe to a *topic* (a unique name) on your phone, and the
app sends alerts to that topic.

### Step 1: Install the app

- **Android:** [Google Play Store](https://play.google.com/store/apps/details?id=io.heckel.ntfy)
- **iOS:** [Apple App Store](https://apps.apple.com/app/ntfy/id1625396347)

### Step 2: Find your topic

1. Open the Home Energy Manager dashboard.
2. Go to **Settings** → scroll to **Notifications** → **ntfy Push Notifications**.
3. You will see a topic name auto-generated from your inverter serial, e.g.
   `hem-SA12345678`.
4. **Copy the topic name.**

### Step 3: Subscribe in the app

1. Open the ntfy app on your phone.
2. Tap the **+** button (or "Subscribe to topic").
3. Paste your topic name (e.g. `hem-SA12345678`).
4. Tap **Subscribe**.

That's it — you will now receive push notifications from your inverter.

### Using your own server

If you prefer to run your own ntfy server for privacy or control, enter its
URL in the **Server** field on the Settings page (defaults to the free
`https://ntfy.sh`). See [ntfy documentation](https://docs.ntfy.sh/) for
self-hosting instructions.

### Troubleshooting

- **No notifications** — Make sure you are subscribed to the exact topic
  shown in the Settings page (case-sensitive).
- **Topic is empty** — The topic is generated from your inverter serial.
  If the app hasn't connected to an inverter yet, no topic will be shown.
  Connect to an inverter first, then revisit Settings.
- **Android battery optimisation** — On Android, make sure ntfy is excluded
  from battery optimisation so it can receive notifications reliably.

---

## Pushover

[Pushover](https://pushover.net) is a paid-once-per-platform notification
service (US$5 per platform — iOS, Android, or desktop). It is a credentialed
service: you register an application to get an **App API Token**, and you
have a **User Key** on your account. Two credentials, similar in spirit to
Telegram's bot token + chat ID pair.

> **Free for the first 10,000 messages per month** per account — comfortably
> above the volume an inverter-alert app would generate.

### Step 1: Create an application

1. Sign up at [pushover.net](https://pushover.net) (if you haven't already).
2. Go to <https://pushover.net/apps/build> and create a new application
   (e.g. name it "Home Energy Manager").
3. You'll be given an **App API Token** (a long string).
4. Your **User Key** is shown on the main dashboard after you log in.

**Copy both** — you will paste them into the app.

### Step 2: Install the app

- **Android:** [Google Play Store](https://play.google.com/store/apps/details?id=net.superblock.pushover)
- **iOS:** [Apple App Store](https://apps.apple.com/app/pushover/id506088175)
- **Desktop:** [pushover.net/downloads](https://pushover.net/downloads)

### Step 3: Enter them in the app

1. Open the Home Energy Manager dashboard.
2. Go to **Settings** → scroll to **Notifications** → **Pushover**.
3. Paste the **App API Token** and **User Key**.
4. Click **Save Notification Settings** — then click
   **Send Test Notification** to verify.

### Troubleshooting

- **"Pushover API 400"** — Double-check both the App API Token and User Key
  are pasted correctly (no surrounding whitespace).
- **No notification** — Make sure the Pushover app is installed on at least
  one device and logged into the same account as your User Key.

---

## Both together

You can enable Telegram, ntfy, and Pushover at the same time — or any
combination. They work independently — if one service has a temporary issue,
you will still get alerts through the others.
