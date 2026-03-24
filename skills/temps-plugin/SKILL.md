---
name: temps-plugin
description: |
  Build external plugins for the Temps deployment platform. Use when the user wants to create, modify,
  or debug a Temps plugin binary — a standalone Rust process that communicates with Temps over a Unix
  domain socket. Also use when the user mentions "temps plugin", "external plugin", "plugin binary",
  "plugin for temps", "plugin UI", or asks about plugin architecture, plugin events, plugin manifest,
  or plugin SDK. Covers the full lifecycle: project scaffolding, manifest, router, events, SQLite
  persistence, embedded React UI, build.rs, testing, and deployment into the plugins directory.
---

# Temps Plugin Development

Build external plugins as standalone Rust binaries that Temps discovers, spawns, and proxies to.

## Architecture Overview

```
Temps (main process)
  ├── Scans ~/.temps/plugins/ for binaries
  ├── Spawns each binary with --socket-path, --auth-secret, --data-dir
  ├── Reads JSON manifest from stdout (handshake phase 1)
  ├── Reads ready signal from stdout (handshake phase 2)
  ├── Opens WebSocket to plugin's /_temps/channel (bidirectional data access)
  ├── Proxies /api/x/{plugin_name}/* → Unix socket
  ├── Serves plugin UI at /api/x/{plugin_name}/ui/*
  └── Delivers platform events over the WebSocket channel
      (plugin exits if WebSocket not connected within 30s — ensure Temps is running)
```

Plugins are **self-contained binaries**. They own their own HTTP routes (axum Router), optional React UI (embedded via `include_dir`), and SQLite database (via sea-orm in their `data_dir`).

## Critical Rules

### NEVER
- Register a `/health` route — the SDK runtime already provides one. Axum panics on `Router::merge` with duplicate routes.
- Use `rt.block_on()` directly inside `router()` — it deadlocks. Use `tokio::task::block_in_place(|| Handle::current().block_on(...))` instead.
- Use `#[tokio::main]` — the SDK creates its own runtime via `run_plugin()`.
- Access the Temps database directly — use `ctx.temps()` for platform data queries over the WebSocket channel.
- Use `sea-orm` with the main Temps database — plugins get their own SQLite in `data_dir`.
- Return `anyhow::Result` — use typed error enums with `thiserror`.
- Use `.unwrap()` or `.expect()` in production paths.

### ALWAYS
- Use `temps_plugin_sdk::main!(YourPlugin)` as the entry point.
- Implement `ExternalPlugin` trait with `manifest()` and `router()` at minimum.
- Use `block_in_place` for any async initialization inside `router()`.
- Embed the UI with `include_dir!("$CARGO_MANIFEST_DIR/web/dist")` and serve via own routes.
- Keep tests in the same file as the code they test (`#[cfg(test)] mod tests`).
- Run `cargo check -p your-plugin` after every modification.
- Run `cargo test -p your-plugin` to verify tests pass.

## Project Structure

```
examples/your-plugin/
├── Cargo.toml
├── build.rs              # Builds web UI (bun + vite), creates fallback in debug
├── src/
│   ├── main.rs           # Plugin struct, manifest, router, on_event, UI handlers, entry point
│   ├── db.rs             # SQLite persistence (sea-orm entities + raw DDL migrations)
│   ├── types.rs          # Shared types (Settings, API DTOs) — all serde(rename_all = "camelCase")
│   └── ...               # Additional modules as needed
└── web/                  # React UI (Vite + TypeScript)
    ├── package.json
    ├── vite.config.ts    # base: "/api/x/{plugin_name}/ui/"
    ├── tsconfig.json
    ├── index.html
    └── src/
        ├── main.tsx
        ├── App.tsx
        ├── api.ts        # API_BASE = "/api/x/{plugin_name}"
        ├── types.ts
        ├── router.ts     # Hash-based routing with useSyncExternalStore
        ├── styles.css
        └── components/
```

## Step-by-Step: Creating a New Plugin

### 1. Cargo.toml

```toml
[package]
name = "temps-your-plugin"
version = "0.1.0"
edition = "2021"
publish = false

[[bin]]
name = "temps-your-plugin"
path = "src/main.rs"

[dependencies]
temps-plugin-sdk = { path = "../../crates/temps-plugin-sdk" }
axum = { version = "0.8" }
sea-orm = { workspace = true }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["full"] }
tracing = "0.1"
chrono = { version = "0.4", features = ["serde"] }
thiserror = { workspace = true }
include_dir = "0.7"
mime_guess = "2.0"
# Add reqwest, scraper, url, uuid etc. as needed

[dev-dependencies]
tempfile = "3"
```

Add the crate to the workspace `Cargo.toml` members list:
```toml
members = [
    # ...existing...
    "examples/your-plugin",
]
```

### 2. build.rs

Copy from the reference implementation. Key behavior:
- **Debug mode** (default): Skips web build, creates fallback `web/dist/index.html` so `include_dir!` doesn't fail.
- **Release mode** (or `FORCE_WEB_BUILD=1`): Runs `bun install` + `bun run build`.

```rust
use std::env;
use std::path::Path;
use std::process::Command;

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let web_dir = Path::new(&manifest_dir).join("web");
    let dist_dir = web_dir.join("dist");

    println!("cargo:rerun-if-changed=web/src");
    println!("cargo:rerun-if-changed=web/index.html");
    println!("cargo:rerun-if-changed=web/vite.config.ts");
    println!("cargo:rerun-if-changed=web/package.json");
    println!("cargo:rerun-if-env-changed=FORCE_WEB_BUILD");

    let profile = env::var("PROFILE").unwrap_or_default();
    if profile == "debug" && env::var("FORCE_WEB_BUILD").is_err() {
        println!("cargo:warning=Skipping plugin web build in debug mode (use FORCE_WEB_BUILD=1 to build)");
        let _ = std::fs::create_dir_all(&dist_dir);
        let fallback = dist_dir.join("index.html");
        if !fallback.exists() {
            let _ = std::fs::write(&fallback, r#"<!DOCTYPE html>
<html><head><meta charset="utf-8"><title>Plugin (dev)</title></head>
<body style="font-family:system-ui;padding:2rem;color:#a1a1aa;background:#09090b;text-align:center">
<h2>Plugin UI not built</h2>
<p>Run <code style="color:#3b82f6">cd examples/your-plugin/web && bun install && bun run build</code></p>
<p>Or set <code style="color:#3b82f6">FORCE_WEB_BUILD=1</code> before cargo build.</p>
</body></html>"#);
        }
        return;
    }

    if !web_dir.join("node_modules").exists() {
        let status = Command::new("bun").arg("install").current_dir(&web_dir).status()
            .expect("Failed to run `bun install`. Is bun installed?");
        if !status.success() { panic!("bun install failed"); }
    }

    let status = Command::new("bun").args(["run", "build"]).current_dir(&web_dir).status()
        .expect("Failed to run `bun run build`. Is bun installed?");
    if !status.success() { panic!("Vite build failed"); }

    assert!(dist_dir.join("index.html").exists(), "Vite build did not produce dist/index.html");
}
```

### 3. main.rs — Plugin Definition

```rust
mod db;
mod types;

use axum::body::Body;
use axum::extract::{Json, Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use include_dir::{include_dir, Dir};
use std::sync::Arc;
use temps_plugin_sdk::prelude::*;

use crate::db::YourStore;
use crate::types::*;

static UI_DIST: Dir = include_dir!("$CARGO_MANIFEST_DIR/web/dist");

pub fn ui_dist() -> &'static Dir<'static> {
    &UI_DIST
}

struct YourPlugin;

impl Default for YourPlugin {
    fn default() -> Self { Self }
}

impl ExternalPlugin for YourPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest::builder("your-plugin", "0.1.0")
            .display_name("Your Plugin")
            .description("What it does")
            .requires_db(false)
            .nav(NavEntry {
                label: "Your Plugin".into(),
                icon: "puzzle".into(),          // Lucide icon name
                section: NavSection::Platform,
                path: "/your-plugin".into(),    // Sidebar route
                order: 50,
            })
            .event("deployment.succeeded")      // Subscribe to events (optional)
            .build()
    }

    fn router(&self, ctx: PluginContext) -> axum::Router {
        // Async init MUST use block_in_place — plain block_on deadlocks!
        let store = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(
                YourStore::open(ctx.data_dir())
            )
        }).expect("Failed to open store");

        let state = Arc::new(AppState { store });

        axum::Router::new()
            .route("/settings", get(get_settings).patch(update_settings))
            // ... your API routes ...
            // UI routes — embedded React SPA
            .route("/ui", get(redirect_to_ui))
            .route("/ui/", get(serve_ui_index))
            .route("/ui/{*path}", get(serve_ui_asset))
            // DO NOT add /health — SDK already provides it (axum panics on duplicate routes)!
            .with_state(state)
            // WARNING: router() runs inside a tokio runtime — block_on() deadlocks.
            // Always use block_in_place(|| Handle::current().block_on(...)) as shown above.
    }

    fn on_event(&self, _ctx: &PluginContext, event: temps_core::external_plugin::PluginEvent) {
        if event.event_type != "deployment.succeeded" { return; }
        // Handle event — spawn a background task for async work
        tokio::spawn(async move {
            // ...
        });
    }
}

temps_plugin_sdk::main!(YourPlugin);
```

### 4. UI Serving Handlers

Standard boilerplate for all plugins — `redirect_to_ui` (301 → `ui/`), `serve_ui_index`, `serve_ui_asset` (with SPA fallback), and `serve_embedded_file` (mime detection + cache headers: `no-cache` for index.html, immutable for assets):

```rust
async fn redirect_to_ui() -> Response {
    Response::builder()
        .status(StatusCode::MOVED_PERMANENTLY)
        .header(header::LOCATION, "ui/")
        .body(Body::empty())
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn serve_ui_index() -> Response { serve_embedded_file(ui_dist(), "index.html") }

async fn serve_ui_asset(Path(path): Path<String>) -> Response {
    let dist = ui_dist();
    if dist.get_file(&path).is_some() { return serve_embedded_file(dist, &path); }
    serve_embedded_file(dist, "index.html")  // SPA fallback
}

fn serve_embedded_file(dist: &Dir<'static>, path: &str) -> Response {
    match dist.get_file(path) {
        Some(file) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream().to_string();
            let cache = if path == "index.html" { "no-cache" }
                       else { "public, max-age=31536000, immutable" };
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, mime)
                .header(header::CACHE_CONTROL, cache)
                .body(Body::from(file.contents()))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
        None => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from("404 Not Found"))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
    }
}
```

### 5. SQLite Persistence (db.rs)

Use sea-orm with raw DDL migrations (not sea-orm-migration crate):

```rust
use sea_orm::{entity::prelude::*, ConnectOptions, Database, DatabaseConnection, Statement};
use std::path::Path;
use std::sync::Arc;

pub struct YourStore {
    db: Arc<DatabaseConnection>,
}

impl YourStore {
    pub async fn open(data_dir: &Path) -> Result<Self, StoreError> {
        let db_path = data_dir.join("your-plugin.db");
        let url = format!("sqlite://{}?mode=rwc", db_path.display());

        let mut opts = ConnectOptions::new(&url);
        opts.max_connections(1).sqlx_logging(false);   // SQLite is single-writer

        let db = Database::connect(opts).await
            .map_err(|e| StoreError::Connect { path: db_path.display().to_string(), reason: e.to_string() })?;

        Self::migrate(&db).await?;
        Ok(Self { db: Arc::new(db) })
    }

    async fn migrate(db: &DatabaseConnection) -> Result<(), StoreError> {
        db.execute(Statement::from_string(sea_orm::DatabaseBackend::Sqlite, r#"
            CREATE TABLE IF NOT EXISTS your_table (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
        "#)).await.map_err(|e| StoreError::Migration(e.to_string()))?;
        Ok(())
    }
}
```

Define sea-orm entities in the same file:

```rust
pub mod your_entity {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "your_table")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub name: String,
        pub created_at: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
    impl ActiveModelBehavior for ActiveModel {}
}
```

### 6. Error Handling

```rust
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("Failed to connect to SQLite at {path}: {reason}")]
    Connect { path: String, reason: String },
    #[error("Migration failed: {0}")]
    Migration(String),
    #[error("Database error: {0}")]
    Database(String),
}

enum AppError {
    Store(StoreError),
    BadRequest(String),
    Internal(String),
}

impl From<StoreError> for AppError {
    fn from(e: StoreError) -> Self { AppError::Store(e) }
}

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        let (status, message) = match self {
            AppError::Store(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg),
            AppError::Internal(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg),
        };
        (status, Json(serde_json::json!({ "error": message }))).into_response()
    }
}
```

### 7. Types (types.rs)

All API types use `serde(rename_all = "camelCase")` to match JavaScript conventions:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginSettings {
    pub some_setting: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UpdateSettings {
    pub some_setting: Option<String>,
    pub enabled: Option<bool>,
}
```

### 8. React UI (web/)

**vite.config.ts** — `base` MUST be `/api/x/{plugin_name}/ui/` (with trailing slash) or JS/CSS assets will 404:

```ts
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  base: "/api/x/your-plugin/ui/",    // Must match plugin name!
  build: { outDir: "dist", emptyOutDir: true },
  server: {
    port: 5175,
    proxy: {
      "/api/x/your-plugin": {
        target: "http://localhost:8081",
        changeOrigin: true,
      },
    },
  },
});
```

**api.ts** — All API calls use absolute paths:

```ts
const API_BASE = "/api/x/your-plugin";

async function request<T>(path: string, options?: RequestInit): Promise<T> {
  const res = await fetch(`${API_BASE}${path}`, {
    ...options,
    headers: { "Content-Type": "application/json", ...options?.headers },
  });
  if (!res.ok) throw new Error(await res.text() || res.statusText);
  if (res.status === 204) return null as T;
  return res.json();
}
```

**router.ts** — Hash-based routing (plugins run in an iframe). Cache the parsed route in `getSnapshot()` — `useSyncExternalStore` compares by reference (`Object.is`), so returning a new object each time causes infinite re-renders:

```ts
let cachedHash = "";
let cachedRoute: Route = { kind: "list" };

function getSnapshot(): Route {
  const hash = window.location.hash;
  if (hash !== cachedHash) { cachedHash = hash; cachedRoute = parseHash(hash); }
  return cachedRoute;
}
```

### 9. Testing

Tests go in `#[cfg(test)] mod tests` at the bottom of each source file:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn test_store() -> (YourStore, TempDir) {
        let dir = TempDir::new().expect("create temp dir");
        let store = YourStore::open(dir.path()).await.expect("open store");
        (store, dir)  // TempDir must live as long as the store
    }

    #[tokio::test]
    async fn test_crud_operations() {
        let (store, _dir) = test_store().await;
        // ... test create, read, update, delete
    }

    #[tokio::test]
    async fn test_settings_defaults() {
        let (store, _dir) = test_store().await;
        let settings = store.get_settings().await.unwrap();
        assert_eq!(settings.some_setting, "default_value");
    }
}
```

**Validate:** `cargo test -p temps-your-plugin` passes all tests. Run `cargo check -p temps-your-plugin` after every modification.

## Build & Deploy

```bash
cargo check -p temps-your-plugin                     # check compilation
cargo test -p temps-your-plugin                      # run tests
cargo build -p temps-your-plugin                     # build without UI (fast)
FORCE_WEB_BUILD=1 cargo build -p temps-your-plugin   # build with embedded UI

# Symlink into plugins directory for local dev
ln -sf $(pwd)/target/debug/temps-your-plugin crates/temps-cli/temps_data/plugins/
```

**After rebuilding:** Restart Temps to pick up the new binary — the symlink points to `target/debug/...` but Temps keeps the old process running. Set `RUST_LOG=temps_external_plugins=debug` to see plugin stderr in Temps logs.

**Validate:** After restart, the plugin appears in the console sidebar and `curl localhost:3000/api/x/your-plugin/health` returns OK.

## Available Platform Events

Subscribe via `.event("event_name")` in the manifest builder:

| Event | Data fields | Description |
|---|---|---|
| `deployment.succeeded` | `url`, `deployment_id`, `project_id`, `environment_id`, `environment_name` | Fires after proxy confirms routes are loaded |
| `deployment.failed` | `deployment_id`, `project_id`, `environment_id`, `error` | Deployment pipeline failed |

Events are delivered over the WebSocket channel and fall back to HTTP `POST /_events`.


## Reference Implementations

- **SEO Analyzer** (full-featured with UI): `examples/example-plugin/`
- **IndexNow** (full-featured with UI): `examples/indexnow-plugin/`
