# Temps Web UI

React-based web interface for Temps, built with Rsbuild and TypeScript.

## Setup

Install the dependencies (we use Bun):

```bash
bun install
```

## Development

Start the dev server:

```bash
bun run dev
```

This will start the development server at http://localhost:3000 with:
- Hot module replacement
- Proxy to API at http://localhost:8081

## Building

### Standalone Build

Build the app for production:

```bash
bun run build
```

This creates a `dist/` directory with the production build.

### Integrated Build (Recommended)

The web UI is **automatically built** as part of the Rust build process:

```bash
# From workspace root
cargo build --release
```

This runs the web build and places output in `../crates/temps-cli/dist/` for easy serving by the Rust binary.

**Build Modes:**
- **Debug**: Web build is skipped by default (use `FORCE_WEB_BUILD=1` to include)
- **Release**: Web build is included automatically (use `SKIP_WEB_BUILD=1` to skip)

See [../crates/temps-cli/BUILD_WEB.md](../crates/temps-cli/BUILD_WEB.md) for details.

## Preview

Preview the production build locally:

```bash
bun run preview
```

## Code Quality

```bash
# Lint
bun run lint

# Format
bun run format

# Type check
bun run lint  # includes tsc --noEmit
```
