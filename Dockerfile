# =============================================================================
# Stage 1: Build frontend (React + Vite)
# =============================================================================
FROM node:22-bookworm AS frontend

WORKDIR /app

# Copy dependency manifests first for caching
COPY package*.json ./
RUN npm ci

# Copy source and build
COPY tsconfig*.json vite.config.ts index.html ./
COPY src/ ./src/
COPY public/ ./public/
RUN npm run build

# =============================================================================
# Stage 2: Build Rust backend (Tauri-based binary)
# =============================================================================
FROM rust:bookworm AS backend

WORKDIR /app

# Install Tauri build dependencies
RUN apt-get update && apt-get install -y \
    libwebkit2gtk-4.1-dev \
    libgtk-3-dev \
    libayatana-appindicator3-dev \
    librsvg2-dev \
    libssl-dev \
    pkg-config \
    && rm -rf /var/lib/apt/lists/*

# Copy Rust project
COPY src-tauri/ ./src-tauri/
# Copy the frontend dist so the build has it available (for bundled resources)
COPY --from=frontend /app/dist/ ./dist/

# Build release binary
RUN cargo build --release --manifest-path src-tauri/Cargo.toml && \
    cp src-tauri/target/release/givenergy-local /usr/local/bin/givenergy-local

# =============================================================================
# Stage 3: Runtime image
# =============================================================================
FROM debian:bookworm-slim

# Install runtime shared libraries needed by the Tauri binary
# Even in headless mode, the binary links to GTK/WebKit/GStreamer/etc.
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libgtk-3-0 \
    libwebkit2gtk-4.1-0 \
    libjavascriptcoregtk-4.1-0 \
    libayatana-appindicator3-1 \
    libsoup-3.0-0 \
    libglib2.0-0 \
    libcairo2 \
    libpango-1.0-0 \
    libgdk-pixbuf-2.0-0 \
    libdbus-1-3 \
    libgstreamer1.0-0 \
    libgstreamer-plugins-base1.0-0 \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

# Copy binary
COPY --from=backend /usr/local/bin/givenergy-local /usr/local/bin/givenergy-local

# Copy frontend dist to a known system path (binary's fallback search path)
COPY --from=frontend /app/dist/ /usr/share/givenergy-local/dist/

# Settings and history persist via volume
VOLUME /root/.givenergy-local

EXPOSE 7337

ENTRYPOINT ["givenergy-local", "--headless"]
