# Contributing to Temps

Thank you for your interest in contributing to Temps. Whether you are reporting a bug, suggesting a feature, improving documentation, or writing code, your contributions are welcome and appreciated.

## Ways to Contribute

- **Bug Reports**: Open an issue with a clear description, steps to reproduce, and expected vs. actual behavior.
- **Feature Requests**: Open an issue describing the use case and proposed solution.
- **Pull Requests**: Fix bugs, implement features, or improve documentation.
- **Discussions**: Join conversations in GitHub Issues and Discussions to help shape the project.

## Development Setup

### Prerequisites

- **Rust** 1.70 or later (`rustup` recommended)
- **Docker** (for container runtime, integration tests, and database)
- **PostgreSQL** with TimescaleDB extension
- **Bun** 1.3.14 (for frontend development)
- **Node.js** 18+
- **C/C++ build toolchain**, **CMake**, and **pkg-config** -- required by native Rust dependencies
- **OpenSSL development headers** -- required by Pingora/OpenSSL and Git dependencies
- **protobuf compiler** (`protoc`) -- required by the `temps-otel` crate to compile OpenTelemetry `.proto` files
- **rustfmt** -- required by generated-code build scripts
- **wasm-pack** 0.15.0 -- required to build the `temps-captcha-wasm` crate
- **wasm32-unknown-unknown Rust target** -- required by the `temps-captcha-wasm` crate

#### Amazon Linux 2023 build prerequisites

```bash
sudo dnf groupinstall -y "Development Tools"
sudo dnf install -y \
  rust cargo rustfmt rust-std-static-wasm32-unknown-unknown \
  cmake pkgconf-pkg-config openssl-devel protobuf-compiler \
  perl-FindBin perl-IPC-Cmd perl-File-Compare perl-File-Copy perl-Time-Piece

cargo install wasm-pack --version 0.15.0 --locked
```

If `cargo install` places `wasm-pack` in `~/.cargo/bin`, make sure that directory is on your `PATH`.

#### Debian/Ubuntu build prerequisites

```bash
sudo apt-get update
sudo apt-get install -y \
  build-essential cmake pkg-config libssl-dev protobuf-compiler

rustup component add rustfmt
rustup target add wasm32-unknown-unknown
cargo install wasm-pack --version 0.15.0 --locked
```

#### macOS build prerequisites

```bash
brew install cmake openssl pkg-config protobuf

rustup component add rustfmt
rustup target add wasm32-unknown-unknown
cargo install wasm-pack --version 0.15.0 --locked
```

### Clone and Build

```bash
git clone https://github.com/gotempsh/temps.git
cd temps

# Build the WASM captcha module (required before workspace compilation)
cd crates/temps-captcha-wasm
bun run build
cd ../..

cargo build --release
```

### Frontend

```bash
cd web
bun install
bun run dev
```

The frontend dev server proxies API requests to the Rust backend.

### Database

Start a TimescaleDB instance with Docker:

```bash
docker volume create temps-db-data

docker run -d \
  --name temps-db \
  -p 5432:5432 \
  -v temps-db-data:/home/postgres/pgdata/data \
  -e POSTGRES_USER=temps \
  -e POSTGRES_PASSWORD=temps \
  -e POSTGRES_DB=temps \
  timescale/timescaledb-ha:pg18
```

### Run the Server

```bash
cargo run --bin temps -- serve --database-url "postgresql://temps:temps@localhost:5432/temps"
```

### Pre-commit Hooks

Set up git hooks to enforce formatting, linting, and commit message conventions:

```bash
./scripts/setup-hooks.sh
```

## Architecture Overview

Temps is organized as a Cargo workspace with 30+ crates, each focused on a specific domain.

### Three-Layer Architecture

```
HTTP Handlers  ->  Service Layer  ->  Data Access (Sea-ORM)
```

- **HTTP Handlers**: Request/response handling, validation, OpenAPI documentation (utoipa).
- **Service Layer**: Business logic, orchestration, transactions.
- **Data Access**: Database queries via Sea-ORM entities and migrations.

### Key Technologies

| Layer | Technology |
|-------|------------|
| Backend | Rust, Axum, Sea-ORM |
| Frontend | React 19, TypeScript, Tailwind CSS, shadcn/ui |
| Proxy | Cloudflare Pingora |
| Containers | Bollard (Docker API) |
| Database | PostgreSQL + TimescaleDB |
| Build (frontend) | Rsbuild, Bun |

### Notable Crates

- `temps-core` -- shared types and utilities
- `temps-deployer` -- Docker/container deployment runtime
- `temps-proxy` -- reverse proxy with TLS/ACME support
- `temps-auth` -- authentication and permission system
- `temps-providers` -- external service providers (PostgreSQL, Redis, S3)
- `temps-otel` -- OpenTelemetry ingest and query (OTLP/protobuf, requires `protoc`)

## Coding Standards

### Conventional Commits

All commit messages must follow [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>[optional scope]: <description>
```

Types: `feat`, `fix`, `docs`, `style`, `refactor`, `perf`, `test`, `build`, `ci`, `chore`, `revert`

Examples:

```
feat(auth): add JWT token refresh
fix(api): handle null response from external service
docs: update installation instructions
```

Use `!` after the type/scope for breaking changes (`feat(api)!: …`). Choose a
meaningful **scope** — it becomes the bold prefix in the changelog.

### Developer Certificate of Origin (sign-off required)

Every commit must be signed off under the [Developer Certificate of Origin](DCO)
(DCO 1.1, the same one the Linux kernel uses). Signing off means you are
certifying that you wrote the code, or that you have the right to submit it
under this project's licenses. It is **not** a copyright assignment — you keep
the copyright to your contribution.

In practice it is one flag:

```bash
git commit -s -m "feat(auth): add JWT token refresh"
```

which appends a trailer matching your git author identity:

```
Signed-off-by: Your Name <your@email.example>
```

Make it the default for every repository you work in:

```bash
git config --global format.signOff true
```

The **DCO** CI job checks every commit in the pull request and fails if any of
them is missing the trailer or if the trailer does not match the commit author.
If you forgot, sign off the whole branch retroactively and force-push:

```bash
git rebase --signoff origin/main
git push --force-with-lease
```

Merge commits and bot-authored commits (Dependabot) are exempt.

### Changelog (generated — do not edit)

`CHANGELOG.md` is a **generated artifact**, produced from Conventional Commits by
[git-cliff](https://git-cliff.org) (config: [`cliff.toml`](./cliff.toml)). **Do not
hand-edit it in a PR** — your commit messages *are* the changelog entries. This is
deliberate: when every PR edited the same `[Unreleased]` section by hand, every PR
conflicted on that file. Generating it eliminates that entire class of conflict.

- A non-conventional commit is **dropped** from the changelog — another reason the
  commit format is enforced.
- The **Changelog** CI job posts a preview comment showing what your PR will add.
- Preview locally with `scripts/changelog.sh --unreleased`.
- Merge commits, release version bumps, and `chore(deps|pr|pull)` are excluded by
  design (see `commit_parsers` in `cliff.toml`).

### Rust

- Run `cargo check --lib` after every change.
- All new code must include tests. Tests must pass before submitting a PR.
- No compiler warnings allowed on new code.
- Use structured logging (`LogLevel::Info`, `LogLevel::Error`, etc.) -- never plain text logging.
- Use `map_err` instead of `context` for error handling to preserve error details.
- Follow the three-layer architecture: handlers call services, services access the database.
- Never access the database directly from HTTP handlers.

### Frontend

- Use React Hook Form with Zod validation for all forms.
- Use React Query for data fetching and caching.
- Never use IFEs (Immediately Invoked Function Expressions) in JSX.
- All hooks must be called before any early returns in components.
- Provide visual feedback for all user actions (loading states, success/error messages).

### Testing

```bash
# Run unit tests
cargo test --lib

# Run tests for a specific crate
cargo test --lib -p temps-deployments

# Run frontend tests
cd web && bun run test
```

Docker-dependent tests run as part of the normal test suite and skip gracefully when Docker is unavailable.

## Pull Request Process

1. **Fork** the repository and create a branch from `main`.
2. **Name your branch** descriptively: `feat/add-webhook-support`, `fix/deployment-timeout`.
3. **Write your code** following the coding standards above.
4. **Add tests** for any new functionality.
5. **Commit** using Conventional Commits format, and sign off each commit with `git commit -s` (see [Developer Certificate of Origin](#developer-certificate-of-origin-sign-off-required)).
6. **Push** your branch and open a Pull Request targeting `main`.
7. **Describe your changes** in the PR body: what changed, why, and how to test it.

Pre-commit hooks run automatically on each commit to check formatting (`cargo fmt`), linting (`cargo clippy`), and commit message format. If a hook fails, fix the issue and commit again.

### PR Checklist

- [ ] Code compiles without warnings (`cargo check --lib`)
- [ ] Tests pass (`cargo test --lib`)
- [ ] New functionality includes tests
- [ ] Commit messages follow Conventional Commits (they generate the changelog — see below)
- [ ] Every commit is signed off (`git commit -s`) per the [DCO](DCO)
- [ ] PR description explains the change
- [ ] Do **not** edit `CHANGELOG.md` — it is generated from your commit messages

## Good First Issues

If you are new to the project, look for issues labeled [`good first issue`](https://github.com/gotempsh/temps/labels/good%20first%20issue). These are scoped tasks that provide a good introduction to the codebase.

## Code of Conduct

This project follows a Code of Conduct to ensure a welcoming and inclusive community. Please read [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) before participating.

## License

Temps is dual-licensed under the [MIT License](LICENSE-MIT) and [Apache License 2.0](LICENSE). By contributing, you agree that your contributions will be licensed under the same terms, and you certify their origin under the [Developer Certificate of Origin](DCO) by signing off each commit.

Forks may rename, rebrand, modify, and redistribute Temps under either license.
When redistributing Temps code, retain its SPDX copyright and license headers
and the notices required by the license you choose. You may add a copyright
notice for your own modifications alongside the existing notices.

CI verifies attribution on first-party source files. If you add a supported
source file without a header, apply it with:

```bash
python3 scripts/source_attribution.py annotate path/to/file
```
