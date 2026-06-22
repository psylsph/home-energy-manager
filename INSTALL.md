# Installation Guide

How to install, update, and run Home Energy Manager on different platforms. For advanced setups (headless server, Docker, unRAID, multi-instance), see the relevant section below.

---

## Using the App on Your Phone

Don't want to be stuck at your computer to check on your solar? You can open the app on your phone too. It's a web page, so there's nothing extra to install on the phone — just point your phone's browser at it and you're done. This section assumes you're running the app on a Windows PC that's left turned on (not in headless/server mode).

### What you'll need

- A Windows PC running Home Energy Manager, **left turned on**
- Your phone connected to the **same Wi-Fi network** as that PC (e.g. both on `home-wifi`, not one on Wi-Fi and one on mobile data)
- The **IP address** of the PC (we'll show you how to find this in Step 2)

### Step 1: Install the app on your PC

> **Windows security notice:** Windows SmartScreen may warn that the app is from an unknown publisher because it is not code-signed. The installer is scanned clean by VirusTotal. If your antivirus flags it as malware, please report a security vulnerability at <https://github.com/psylsph/home-energy-manager/issues>. If your antivirus flags the app, see [Bypassing the SmartScreen warning](#bypassing-the-smartscreen-warning) below.

1. On your PC, open a web browser and go to the [**Releases page**](https://github.com/psylsph/home-energy-manager/releases/latest)
2. Download the `.msi` file (pick the latest one at the top)
3. Once it finishes downloading, double-click it to run the installer
4. Follow the prompts — the app installs like any other Windows program
5. Launch **Home Energy Manager** from your **Start menu**

You'll see a window pop up showing your inverter data. Leave it open — the phone will be talking to this window in a moment.

> 💡 **Tip:** If you want the app to start automatically when Windows boots, open it once, go to **Settings → App → Start on Login** and turn the toggle on. That way the app's always running, even after a restart. (You can also still manage this from the OS-level **Start → Settings → Apps → Startup** panel — the app reads back whatever is registered there.)

### Step 2: Find your PC's IP address

This is the little number that identifies your computer on your home network. It looks something like `192.168.1.42` or `10.0.0.25`.

**On Windows:**

1. Press the **Windows key + R** to open the Run box
2. Type `cmd` and press **Enter** to open a black command-prompt window
3. Type `ipconfig` and press **Enter**
4. Look for the line that says **IPv4 Address** under your Wi-Fi or Ethernet adapter — that's your number. It'll look like `192.168.1.42`.

> 💡 **Write this number down** — you'll need it in a moment. In the examples below we'll pretend yours is `192.168.1.42`. Replace it with your actual one.

### Step 3: Open it on your phone

1. On your phone, open your web browser (Safari on iPhone, Chrome on Android — any browser works)
2. In the address bar at the top, type: `http://192.168.1.42:7337`
   - Replace `192.168.1.42` with the number you wrote down in Step 2
   - The `:7337` part is the **port** — always include it, with the colon in front
3. Press **Go** or the enter key on your keyboard

You should see the Home Energy Manager dashboard appear, just like on your computer. 🎉

### Step 4: Add it to your home screen (optional, but nice)

This puts a little app-style icon on your phone so you can open it like a regular app:

**On iPhone (Safari):**

1. Tap the **Share button** (the square with the arrow pointing up)
2. Scroll down and tap **Add to Home Screen**
3. Give it a name like "Energy" and tap **Add**

**On Android (Chrome):**

1. Tap the **three dots** (⋮) in the top-right corner
2. Tap **Add to Home screen** or **Install app**
3. Give it a name like "Energy" and tap **Add**

Now you can open it with one tap, just like a normal app.

### It doesn't work — what now?

**Phone says "can't connect" or "site not found":**

- Double-check you're on the **same Wi-Fi network** as the PC running the app (not on mobile data, not on a guest network, not on a 5GHz vs 2.4GHz split if your router separates them — most modern routers don't, but some do)
- Double-check the IP address. If your PC goes to sleep, or you restart your router, the number can change. Just run through Step 2 again to get the new one.
  - To stop the number changing, open **Settings → Network & Internet → Wi-Fi → your network → IP assignment → Edit**, and switch from **Automatic (DHCP)** to **Manual**. Turn on IPv4 and fill in the same IP address as before. That locks it in.
- Make sure the Home Energy Manager window is still open on your PC (it has to be running for the phone to reach it)
- Some antivirus or firewall programs (Norton, McAfee, even Windows Defender Firewall) can block the connection. If nothing else works, try temporarily disabling your firewall to see if that's the culprit.

**Phone says "this site is not secure" or shows a warning:**

- That's normal and expected. The app talks to your inverter over your local network, so it uses plain `http://` (no `s`). Your data never leaves your house — it's only your phone talking to your PC. You can safely tap "Proceed" or "Visit this website".

**It works at home but not when I'm out:**

- That's expected! When you're out and about, your phone isn't on your home Wi-Fi anymore, so it can't reach the PC. To use the app away from home you'd need to set up a VPN back to your house, or expose the app to the internet (which is more advanced and outside the scope of this guide). The easy and safe option is just to use it on your home Wi-Fi.

**Tip for an always-reliable setup:** Set your PC to **never sleep** while plugged in (**Settings → System → Power → Screen and sleep → When plugged in, PC goes to sleep after → Never**) and turn on the **Startup** toggle for the app (see Step 1). That way the dashboard is always reachable from your phone whenever you're home. For a truly hands-off setup that runs 24/7 without your PC being on, see the [Raspberry Pi (headless server)](#raspberry-pi-headless-server) section below.

---

## Quick Install

### Windows

> **Windows security notice:** Windows SmartScreen may warn that the app is from an unknown publisher because it is not code-signed. The installer is scanned clean by VirusTotal. If your antivirus flags it as malware, please report a security vulnerability at <https://github.com/psylsph/home-energy-manager/issues>.

1. Download the `.msi` file from the [**Releases page**](https://github.com/psylsph/home-energy-manager/releases/latest)
2. Double-click it to run the installer
3. Follow the prompts — the app installs like any other Windows program
4. Launch **Home Energy Manager** from your Start menu

#### "Windows protected your PC" — how to bypass the SmartScreen warning

<a id="bypassing-the-smartscreen-warning"></a>

The first time you run the installer (or the app itself), Windows may pop up a blue full-screen message saying **"Windows protected your PC"** and refuse to open the file. That's Microsoft Defender SmartScreen being cautious because the app isn't code-signed by a known publisher. It doesn't mean the app is dangerous — it just means Microsoft doesn't recognise the developer yet. The installer has been scanned by VirusTotal and comes back clean.

**If you see the blue "Windows protected your PC" screen:**

1. **Don't click "Don't run"** — but don't panic either. Just look at the message for a moment.
2. The **"More info"** link is small and grey, just below the main warning text. **Click that.**
3. A new line will appear at the bottom: **"Run anyway"**. **Click that.**
4. Windows will ask once more if you're sure. Click **Yes** (or **Run**).
5. The installer (or app) will now open normally. You only have to do this once per machine — after that, Windows remembers your choice.

**If Edge keeps flagging the download as "suspicious" or "uncommon":**

When you click the `.msi` link in Microsoft Edge, it may show a banner at the top of the downloads panel saying something like *"This file is not commonly downloaded and could be unsafe."* Or the download might just refuse to start. That's Edge being extra cautious.

1. Click the **three dots (⋯)** next to the warning in the downloads panel
2. Choose **"Keep"** or **"Download anyway"**
3. Edge will save the file. Once saved, follow the blue-screen steps above to actually run it.

**If your antivirus (Norton, McAfee, Bitdefender, Kaspersky, etc.) deletes or quarantines the installer:**

This is the same root cause — the app isn't code-signed, so some security suites get jumpy.

1. Open your antivirus program
2. Find the **quarantine** or **threat history** section
3. Look for an entry named **"givenergy-local"** or **"home-energy-manager"**
4. Choose **"Restore"** or **"Allow"** (the exact wording depends on your antivirus)
5. Add the folder you downloaded the file to (usually `Downloads`) to the antivirus's **exclusions / allowlist** so it doesn't get flagged again
6. Now follow the blue-screen steps above to run it

**If you're still stuck:** see the [FAQ](./FAQ.md) for more help, or open an issue at <https://github.com/psylsph/home-energy-manager/issues> and someone will walk you through it.

### macOS

1. Download the `.dmg` file from the [**Releases page**](https://github.com/psylsph/home-energy-manager/releases/latest)
   - **Apple Silicon** (M1/M2/M3/M4): download the file with `aarch64` in the name
   - **Intel**: download the file with `x64` in the name
2. Open the `.dmg` and drag the app to your **Desktop** or **Home folder**
   - ⚠️ Do **not** drag it to `/Applications` — macOS blocks unsigned apps there
3. On first launch, right-click the app → **Open** → **Open** to bypass Gatekeeper
4. After the first launch, you can open it normally

> See the [FAQ](./FAQ.md) for help with macOS-specific issues.

### Linux

**Ubuntu / Debian / Raspberry Pi OS:**

1. Download the `.deb` file from the [**Releases page**](https://github.com/psylsph/home-energy-manager/releases/latest)
   - Most computers: the file with `amd64` in the name
   - Raspberry Pi (64-bit OS only): the file with `arm64` in the name
2. Install it:

   ```bash
   sudo apt install ./home-energy-manager_*_amd64.deb
   ```

3. Launch from your app menu, or run `givenergy-local` in a terminal

**Fedora / openSUSE:**

1. Download the `.rpm` file from the [**Releases page**](https://github.com/psylsph/home-energy-manager/releases/latest)
2. Install it:

   ```bash
   sudo dnf install ./home-energy-manager-*.rpm    # Fedora
   sudo zypper install ./home-energy-manager-*.rpm  # openSUSE
   ```

#### Linux system requirements

On some Linux distributions you may need to install two system libraries first:

```bash
sudo apt install libwebkit2gtk-4.1-0 librsvg2-2
```

Recent `.deb` packages install these automatically, but older builds don't — if the app fails to launch, try running this command.

> **Raspberry Pi** users: you need a **64-bit operating system** (Raspberry Pi OS 64-bit, Ubuntu Server 64-bit, etc.). The 32-bit OS is not supported.

### Updating

To update, simply download and install the latest version from the [**Releases page**](https://github.com/psylsph/home-energy-manager/releases/latest) — your settings and history are preserved automatically.

### Uninstalling

**Linux (`.deb`):**

```bash
sudo apt purge home-energy-manager
```

This removes the app but keeps your data. To delete your settings and history as well:

```bash
rm -rf ~/.givenergy-local
```

⚠️ **This permanently deletes all your recorded energy history and settings.**

**Windows:** Use **Settings → Apps → Installed apps** to uninstall.

**macOS:** Drag the app to the Bin.

---

## Running Headless (as a Background Service)

You can run Home Energy Manager without a desktop window — it serves the full web UI to any browser on your network. This is ideal for a Raspberry Pi or always-on server.

### Option 1: Native

Build from source and run in the background:

```bash
# 1. Install build dependencies
npm install
cargo install tauri-cli

# 2. Build the frontend
npm run build

# 3. Build the app
cd src-tauri && cargo build --release

# 4. Run headless
nohup ./target/release/givenergy-local --headless > givenergy-local.log 2>&1 &
```

Then open `http://your-machine-ip:7337` in any browser on your network.

If you already have a built `dist/` folder from a previous build, you can point to it:

```bash
./target/release/givenergy-local --headless --dist /path/to/dist
```

#### Raspberry Pi (headless server)

Home Energy Manager runs great on a Raspberry Pi as a dedicated home server. You need **64-bit** Trixie (Debian 13) or newer — older releases (Bookworm, Raspberry Pi OS) ship glibc 2.36 which is too old for the prebuilt binary.

**1. Install the ARM64 `.deb`**

Download the latest `arm64.deb` from the [Releases page](https://github.com/psylsph/home-energy-manager/releases/latest) and install:

```bash
sudo dpkg -i home-energy-manager_*_arm64.deb
```

This installs `givenergy-local` to `/usr/bin/` — it's now on your PATH.

**2. Run headless**

```bash
givenergy-local --headless
```

The web UI is available at `http://<pi-ip>:7337` from any browser on your network.

**3. Auto-start on boot (systemd)**

Create a systemd service so the app starts automatically when the Pi boots:

```bash
sudo tee /etc/systemd/system/givenergy-local.service << 'EOF'
[Unit]
Description=Home Energy Manager
After=network.target

[Service]
Type=simple
ExecStart=/usr/bin/givenergy-local --headless
Restart=on-failure
RestartSec=5
User=pi

[Install]
WantedBy=multi-user.target
EOF

sudo systemctl daemon-reload
sudo systemctl enable --now givenergy-local
```

Check it's running:

```bash
sudo journalctl -u givenergy-local -f
```

### Option 2: Docker

The quickest way to get started with Docker:

```bash
# Build and start
docker compose up -d

# Rebuild after code changes
docker compose build && docker compose up -d
```

Your settings and history are stored in `~/.givenergy-local` on the host machine and survive container restarts.

### Option 3: unRAID

If you use unRAID, you can run Home Energy Manager as a Docker container:

**1. Set up the project folder**

Open the unRAID terminal (Main UI → top-right >_ Terminal):

```bash
mkdir -p /mnt/user/appdata/givenergy-local
cd /mnt/user/appdata/givenergy-local
git clone https://github.com/psylsph/home-energy-manager.git .
```

**2. Create a Dockerfile**

```bash
nano Dockerfile
```

Paste this:

```dockerfile
FROM node:22-bookworm AS frontend
WORKDIR /app
COPY package*.json ./
RUN npm install
COPY . .
RUN npm run build

FROM rust:latest AS builder
WORKDIR /app
RUN apt-get update && apt-get install -y \
    pkg-config \
    libdbus-1-dev \
    libgtk-3-dev \
    libsoup2.4-dev \
    libjavascriptcoregtk-4.1-dev \
    libwebkit2gtk-4.1-dev \
    libayatana-appindicator3-dev \
    librsvg2-dev \
    && rm -rf /var/lib/apt/lists/*
COPY . .
COPY --from=frontend /app/dist ./dist
WORKDIR /app/src-tauri
RUN cargo build --release

FROM debian:trixie-slim
WORKDIR /app
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libgtk-3-0 \
    libwebkit2gtk-4.1-0 \
    libayatana-appindicator3-1 \
    librsvg2-2 \
    libdbus-1-3 \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/src-tauri/target/release/givenergy-local /app/givenergy-local
COPY --from=frontend /app/dist /app/dist
EXPOSE 7337
CMD ["/app/givenergy-local", "--headless"]
```

Save with `Ctrl+O` → `Enter` → `Ctrl+X`.

**3. Build and run**

```bash
docker build --no-cache -t givenergy-local .
```

```bash
docker run -d \
  --name givenergy-local \
  --network host \
  -v /mnt/user/appdata/givenergy-local/data:/root/.givenergy-local \
  --restart unless-stopped \
  givenergy-local
```

Check it's running:

```bash
docker logs -f givenergy-local
```

Visit `http://[YOUR UNRAID IP]:7337` in a browser.

**4. Add to the unRAID Docker UI**

To manage it from the unRAID web interface:

1. Remove the container created above: `docker rm -f givenergy-local`
2. Go to the unRAID **Docker** page → **Add Container** → **Advanced Mode**
3. Set **Repository** to `givenergy-local`
4. Set **Icon URL** to `https://avatars.githubusercontent.com/u/84566103?s=200&v=4`
5. Set **Web UI** to `http://[IP]:7337`
6. Set **Network Type** to **Host**
7. Add a **Container Path** of `/root/.givenergy-local` with **Host Path** `/mnt/user/appdata/givenergy-local/data`

**5. Updating**

```bash
cd /mnt/user/appdata/givenergy-local
git pull
docker build --no-cache -t givenergy-local .
docker rm -f givenergy-local
docker run -d \
  --name givenergy-local \
  --network host \
  -v /mnt/user/appdata/givenergy-local/data:/root/.givenergy-local \
  --restart unless-stopped \
  givenergy-local
```

Your settings and history are preserved — only the app binary is replaced.

---

## Running Multiple Instances

If you have more than one inverter, you can run multiple copies of the app. Each one needs its own config directory and HTTP port.

### Separate config directory

Set `GIVENERGY_LOCAL_CONFIG_DIR` to a different path for each inverter:

**Linux / macOS:**

```bash
# First inverter (default)
./givenergy-local

# Second inverter
GIVENERGY_LOCAL_CONFIG_DIR=~/givenergy-instance2 ./givenergy-local
```

**Windows (PowerShell):**

```powershell
$env:GIVENERGY_LOCAL_CONFIG_DIR = "C:\Users\You\givenergy-config-2"
.\givenergy-local.exe
```

### Separate HTTP port

Each instance needs a different port (default is 7337).

**Desktop app:** Change the port in **Settings → HTTP Port**, then restart.

**Headless:** Use the `--port` flag:

```bash
GIVENERGY_LOCAL_CONFIG_DIR=~/givenergy-server ./givenergy-local --headless --port 8080
```

If two instances share the same port, the second one will fail to start. Change the port before launching.

---

## Building from Source (for Developers)

If you want to contribute or build from source:

```bash
# 1. Install dependencies
npm install
cargo install tauri-cli

# 2. Build the frontend
npm run build

# 3. Build and run
cd src-tauri && cargo tauri dev
```

The frontend must be built before the Rust binary — the server needs the `dist/` folder to serve the UI.

See [DESIGN.md](./DESIGN.md) for full architecture documentation.
