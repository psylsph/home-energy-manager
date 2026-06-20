# Installation Guide

How to install, update, and run Home Energy Manager on different platforms. For advanced setups (headless server, Docker, unRAID, multi-instance), see the relevant section below.

---

## Quick Install

### Windows

> **Windows security notice:** Windows SmartScreen may warn that the app is from an unknown publisher because it is not code-signed. The installer is scanned clean by VirusTotal. If your antivirus flags it as malware, please report a security vulnerability at <https://github.com/psylsph/home-energy-manager/issues>.

1. Download the `.msi` file from the [**Releases page**](https://github.com/psylsph/home-energy-manager/releases/latest)
2. Double-click it to run the installer
3. Follow the prompts — the app installs like any other Windows program
4. Launch **Home Energy Manager** from your Start menu

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
