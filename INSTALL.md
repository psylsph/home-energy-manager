# Installation & Advanced Setup

Detailed instructions for building, running, and deploying Home Energy Manager.

---

## Development

```bash
# 1. Install dependencies
npm install
cargo install tauri-cli

# 2. Build the frontend first (creates dist/)
npm run build

# 3. Build and Run the Rust backend
cd src-tauri && cargo tauri dev
```

## Running Headless (Native)

```bash
# 1. Install dependencies
npm install
cargo install tauri-cli

# 2. Build the frontend first (creates dist/)
npm run build

# 3. Build the Rust backend
cd src-tauri && cargo build --release

# 4. Run headless (no GUI window)
nohup ./target/release/givenergy-local --headless > givenergy-local.log 2>&1 &
```

> The frontend (`dist/`) must be built before the Rust binary, otherwise
> the server won't have any UI files to serve. Alternatively, use `--dist`
> to point to an existing build:
>
> ```bash
> ./target/release/givenergy-local --headless --dist /path/to/dist
> ```

## Running Headless (Docker)

```bash
# Build and start with docker compose
docker compose up -d

# Rebuild after code changes
docker compose build && docker compose up -d
```

**Persistent data** (settings + history DB) lives in `${HOME}/.givenergy-local` and
is mounted into the container at `/root/.givenergy-local`. This survives restarts.

## Running on unRAID

Community contributor instructions for running Home Energy Manager as a Docker container on unRAID.

### 1. Create a folder

Open the unRAID terminal (Main UI → top-right >_ Terminal):

```bash
mkdir -p /mnt/user/appdata/givenergy-local
cd /mnt/user/appdata/givenergy-local
```

### 2. Download the project

```bash
git clone https://github.com/psylsph/home-energy-manager.git .
```

### 3. Create a Dockerfile

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

### 4. Build the image

This will take a few minutes on first run.

```bash
docker build --no-cache -t givenergy-local .
```

### 5. Run the container

```bash
docker run -d \
  --name givenergy-local \
  --network host \
  -v /mnt/user/appdata/givenergy-local/data:/root/.givenergy-local \
  --restart unless-stopped \
  givenergy-local
```

Check the logs:

```bash
docker logs -f givenergy-local
```

Visit `http://[YOUR UNRAID IP]:7337` in a browser to verify it's running.

### 6. Add to the unRAID Docker UI

To make the container manageable from the unRAID Docker page, first remove the manually created one:

```bash
docker rm -f givenergy-local
```

Then in the unRAID **Docker** page:

1. Click **Add Container** → select **Advanced Mode**
2. Set **Repository** to `givenergy-local`
3. Set **Icon URL** to `https://avatars.githubusercontent.com/u/84566103?s=200&v=4`
4. Set **Web UI** to `http://[IP]:7337`
5. Set **Network Type** to `Host`
6. Add a **Container Path** of `/root/.givenergy-local`
7. Set the corresponding **Host Path** to `/mnt/user/appdata/givenergy-local/data`

The container will persist data (settings + history) across stop/start cycles.

### 7. Updating

To update to the latest version, pull the latest code and rebuild:

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

Your settings and history are preserved in the mounted volume — only the binary is replaced.

## Running Multiple Instances

You can run multiple copies of the app to control different inverters. Each instance
needs its own **config directory** and **HTTP port**.

### Step 1: Separate config directory

Set `GIVENERGY_LOCAL_CONFIG_DIR` to a different directory for each instance so they
don't share `settings.json` and `history.db`:

**Linux / macOS:**

```bash
# Default (uses ~/.givenergy-local/)
./givenergy-local

# Second instance with its own config and history
GIVENERGY_LOCAL_CONFIG_DIR=~/givenergy-instance2 ./givenergy-local
```

**Windows (PowerShell):**

```powershell
# Second instance
$env:GIVENERGY_LOCAL_CONFIG_DIR = "C:\Users\You\givenergy-config-2"
.\givenergy-local.exe
```

**Windows (Command Prompt):**

```cmd
set GIVENERGY_LOCAL_CONFIG_DIR=C:\Users\You\givenergy-config-2
givenergy-local.exe
```

### Step 2: Separate HTTP port

Each instance must use a different HTTP port (default 7337).

**Desktop app:** Change the port in **Settings → HTTP Port**, then restart the app.
Alternatively, edit `http_port` directly in the `settings.json` file in the config
directory before launching the second instance:

```json
{
  "http_port": 8080,
  ...
}
```

**Headless server:** Use the `--port` flag:

```bash
GIVENERGY_LOCAL_CONFIG_DIR=~/givenergy-server ./givenergy-local --headless --port 8080
```

**Docker:** Edit `http_port` in the mounted `settings.json`, or use `--port` in the
container command.

If two instances share the same port, the second one will fail to start its web
server and the app window will show a blank page.

---

See [DESIGN.md](./DESIGN.md) for full architecture documentation.
