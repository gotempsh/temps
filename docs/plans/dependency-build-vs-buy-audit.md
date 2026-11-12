# Dependency audit: what Temps could build instead of depending on

Audit of commit `b1f12c448`, run 2026-11-02. 208 direct Rust crates and 135 direct JS packages are scored, plus 1 Python package. No code has changed.

> **This audit is out of date in places.** It ran on a worktree 353 commits behind `main` (`5a9d2e0cb`). The findings below are corrected where I rechecked them. The scorecards, tier counts and method numbers still describe `b1f12c448`, so crates added since then are not scored, and `main`'s tree is 1,131 crates, not 1,137.

### What changed on `main`

| Finding | Status on `main` |
|---|---|
| `config`, `tower-cookies`, `tokenizers`, `tiktoken-rs`, `validator` | **Done:** all gone from `Cargo.lock` |
| No `deny.toml`; unpinned `cargo install wasm-pack` in `rust-tests.yml` | **Done:** both fixed |
| `shlex` not used by Temps | **Wrong now:** `temps-deployer` calls it directly |
| `image` pulls every codec in `temps-auth` | **Still open:** see below |
| Seven unused web packages | **Still open:** still declared, still zero references in `web/` |
| `picomatch-rs` | **Still open:** still used by `temps-presets` |

**Best next step on `main`:** one line in `crates/temps-auth/Cargo.toml` (`features = ["default"]` to `["png"]`) removes 35 crates and takes `Cargo.lock` from 1,377 to 1,339 entries (measured). It also takes `paste` out of the lockfile, so the RUSTSEC-2024-0436 exception can go. This edit has been re-resolved but not compiled on `main`.

## Bottom line

**Rewriting small libraries is mostly not what reduces supply chain risk here.** Of the 43 Rust crates that score 8 or higher, 23 remove nothing, because other dependencies pull them in anyway. Replacing `hex`, `base64`, `urlencoding`, `uuid` and 16 other utilities together removes **0** of the 1,137 crates in the tree.

The biggest gains need no new code:

- **Three manifest lines removed 47 crates at the audited commit.** `config` and `tower-cookies` have since gone from `main`. The third is still open: `temps-auth` pulls every `image` codec, including an AV1 encoder, to write one PNG.
- **Seven unused web packages account for 52 lockfile entries**, including `lodash`.
- **197 dependency declarations look unused** across the workspace.

Across all five tiers below, the Rust tree could go from 1,137 crates to about 848. Only about a third of that reduction (tiers 3 and 4, 95 of 289 crates) needs anything to be built.

**Do you need `shlex`? Not really, but replacing it gains almost nothing.** On `main`, one call in `temps-deployer` uses it. Replacing that call removes `shlex` 2.0.1, but `cc` still pulls 1.3.0 at build time. See [The `shlex` question](#the-shlex-question).

## Decisions for you

1. **Is it acceptable to narrow `jsonschema` to a subset of keywords?** LLM structured output validation is the only use. A subset removes 12 crates, but schemas using unsupported keywords would need a clear error. I would defer this until the smaller tiers are done.
2. **Should `microsandbox` move behind a feature flag or into a separate binary?** It accounts for about 110 crates once the other tiers are done, more than any other decision here, but it changes how ADR-050 ships. I recommend a separate binary if macOS and glibc installs can tolerate it.
3. **Is the Sentry schema replacement worth doing?** It removes 31 crates and a git dependency, but ingest parses untrusted SDK payloads and this is the riskiest build on the list. I recommend doing it after tiers 1 to 3, with golden fixtures.

## What the score means

The score answers one question: **how realistically could Temps build and own this?** It does not say whether we should. Read it with the **Removes** column, because a 9 that removes nothing is not worth doing.

| Score | Meaning | Rust | JS |
|---|---|---|---|
| 8 to 10 | Delete it, swap it for something in the tree, or write it in under about 200 lines | 43 | 27 |
| 5 to 7 | A bounded project: real logic and tests, but well specified | 41 | 23 |
| 1 to 4 | Do not build: crypto, TLS, protocols, parsers of untrusted input, runtimes, or too widely used to be worth it | 120 | 84 |

Each score weighs the API actually used, how hard the problem is, whether it handles untrusted input, whether it needs ongoing data or spec updates, and whether it can be tested against known vectors. **The scores are my judgement**, while the other columns are measured.

**Columns:**

- **Removes**: packages that leave the tree if this dependency goes. Zero means something else still needs it.
- **Refs / files**: code references. This is a name match, so common words such as `url` or `time` can over-count.
- **Owners / 90-day downloads**: public crates.io data at audit time.

**Actions:** Delete (unused), Slim (reduce features), Swap (replace with something already in the tree or in std), Build, Build later, Isolate, Keep.

## What to do, in order

Each tier's count is on top of the tiers above it. Starting tree: 1,137 crates on Linux x86_64.

| Tier | Work | Removes | Tree after | Basis |
|---|---|---:|---:|---|
| 1. Delete and slim | Remove `config` and `tower-cookies`, make `image` PNG-only in `temps-auth`, remove the 197 unreferenced declarations | 53 | 1,084 | 47 measured by re-resolving, the rest modelled |
| 2. Test tools | `axum-test` to `oneshot`, `mockall` to fakes, `mockito` to `wiremock`, drop `env_logger`, `temp-dir`, `maplit` | 31 | 1,053 | Modelled |
| 3. Small builds and swaps | `picomatch-rs`, `slug`, `csv`, `downcast-rs`, `if-addrs`, `gethostname`, `axum-extra`, `cron` to `croner`, `cloudflare`, `prost-build`, `include_dir`, `indicatif`, `colored`, `rustls-pemfile` | 30 | 1,023 | Modelled |
| 4. Larger builds | Sentry `relay-*`, `octocrab`, `headless_chrome`, `jsonschema` | 65 | 958 | Modelled |
| 5. Isolate | `microsandbox` behind a feature flag or separate binary | 110 | 848 | Modelled |

**Compile status of the tier 1 measured cuts:** `cargo check --all-targets` passes on `temps-config`, `temps-auth` and `temps-proxy` with `config` and `tower-cookies` removed, `image` PNG-only, and the separate `sqlx = 0.9` declaration removed from `temps-proxy`. This was run on macOS on a scratch copy. It proves the code compiles, not that it behaves the same: tests have not been run, so run the `temps-auth` MFA QR tests when this lands. The `sqlx` removal changes nothing in the Linux count, because `microsandbox` still needs 0.9.

**Also in tier 1, with no tree change on Linux:**

- **Optional dependencies nothing enables:** remove `tokenizers`, `tiktoken-rs` and `validator`. With the three cuts above, this takes `Cargo.lock` from 1,389 entries to 1,319 (measured).
- **Advisory exceptions (at the audited commit; `main` now needs only the `image` change):** removing `tokenizers` as well as slimming `image` takes `paste` out of `Cargo.lock`, so the RUSTSEC-2024-0436 exception in `dependency-scan.yml` can go. Slimming `image` alone does not do this, because `cargo audit` reads the lockfile and `tokenizers` still locks it.
- **Web console:** remove `@nivo/line`, `@radix-ui/react-hover-card`, `cobe`, `date-fns-tz`, `ghostty-web`, `highlight.js` and `rehype-highlight`. None has a reference in `web/`. The lockfile goes from 844 to 793 entries (measured), but I did not run a web build on it.

**Do `picomatch-rs` first.** It stands out from every other direct crate: 7,900 downloads in 90 days against millions for most others, a single owner, version 0.1.0, and one call that `globset` could cover. That is the risk profile an attacker looks for.

**Other results of tiers 3 and 4:**

- `rustls-pemfile` retires the RUSTSEC-2025-0134 exception.
- `headless_chrome` retires RUSTSEC-2026-0367, 0368 and 0369, and one of the paths behind the zstd exceptions.
- `prost-build` should also retire the `protoc` CI install, once confirmed nothing else needs it.

**One JS outlier:** `promptfoo`, a dev tool in `apps/temps-e2e`, accounts for about 827 of that lockfile's 866 entries. It is not something to rebuild, but running it through `bunx` at a pinned version, or moving it to its own lockfile, would keep those packages out of routine installs.

## The `shlex` question

| Question | Answer |
|---|---|
| Do we depend on it? | Yes, twice on `main`. `temps-deployer` uses 2.0.1 for one `shlex::split` call on Compose string commands (`compose_translation.rs:527`). `cc` separately pulls 1.3.0, and 12 crates use `cc` at build time. |
| Would writing our own remove it? | Only 2.0.1, so 1 crate. 1.3.0 stays through `cc`. Removing that too would take a `[patch.crates-io]`, which leaves the same code running at build time, now maintained by us and with no advisories. |
| Could it be built? | Yes, about 8 out of 10. **I would keep it:** a hand-written splitter could disagree with what users expect from Compose quoting, for a gain of one crate. And "small" does not mean "safe": shlex's own quoting had [RUSTSEC-2024-0006](https://rustsec.org/advisories/RUSTSEC-2024-0006), where `{` and `\xa0` passed through unquoted and could split one argument into several. |

**The real finding is next door.** Temps already has 13 hand-written shell-quoting helpers across `temps-agents`, `temps-ai-chat`, `temps-backup`, `temps-providers` and `temps-sandbox`, and `temps-sandbox` says in its comments that two are duplicated on purpose. I read all 13 and found no bug: each single-quotes, and the two allowlists some of them use leave `{` quoted. But 13 copies mean a future fix has to land 13 times. Merge them into one tested function in `temps-core`, which needs no new dependency. More inline copies exist, for example in `temps-agents/src/services/executor.rs`; I did not review those.

## What not to build

- **Crypto and TLS:** `sha2`, `hmac`, `aes-gcm`, `rsa`, `argon2`, `subtle`, `rustls`, `ed25519-dalek`. These are short to write and very hard to get right, and tests will not reveal a timing leak.
- **Parsers of untrusted input:** `yaml_serde`, `zip`, `tar`, `quick-xml`, `lopdf`, `scraper`, `url`, `hyper`.
- **Anything where the tree does not shrink:** `hex`, `base64`, `percent-encoding`, `uuid`, `log`. The attacker's code would still ship in the binary, so our copy adds work and no safety.

**Rewriting also has a security cost.** Code we own gets no RustSec advisory and no outside review. `cargo audit` is silent on it, so we would have to find our own bugs. AI makes the code cheap to write, not the review and upkeep.

## Controls that protect against more than rewriting does

A rewrite protects against one package. These cover all 1,137:

| Gap | Evidence | Fix |
|---|---|---|
| No delay on new releases | No `minimumReleaseAge` in `web/bunfig.toml`, no `cooldown` in `.github/dependabot.yml` | Wait a few days before adopting a release. The [September 2025 chalk and debug takeover](https://cycode.com/?p=10541) was reported live for only about 2.5 hours. |
| Lockfile not enforced in builds | `cargo build` without `--locked` at `release.yml:277` and `392` among others, and `Dockerfile:120`; `bun install` without `--frozen-lockfile` at `Dockerfile:100` and `106` | Add the flags, so a lockfile mismatch fails the build |
| Only `web/` is audited for JS | `dependency-scan.yml` covers `web/bun.lock` only | Also scan `apps/temps-cli`, `sdks/node` and `telemetry-api` |
| Git dependency pinned by tag | `autopack-*` uses tag `v0.1.6` | Pin to a full SHA, like `temps-agent-runtime`. `Cargo.lock` protects this only if `--locked` is enforced. |

## Scorecard: Rust

### Score 8 to 10

| Score | Library | Removes | Refs / files | Owners / 90-day downloads | Action | Why |
|---|---|---|---|---|---|---|
| 10 | `config` | 11 | 0 / 0 | 5 / 21.7M | Delete | Declared in temps-config and never used. Removes json5, ron, rust-ini and yaml-rust2 parsers with it. Measured: 11 crates. |
| 10 | `tower-cookies` | 1 | 0 / 0 | 1 / 2M | Delete | Declared in temps-auth, referenced nowhere. Measured: 1 crate. |
| 10 | `hex` | 0 | 256 / 128 | 1 / 191M | Keep | Five lines of code, but 24 other crates pull it, so nothing leaves the tree. Not worth 256 edits. |
| 10 | `lazy_static` | 0 | 0 / 0 | 1 / 239.2M | Swap | `std::sync::LazyLock` replaces it. 6 crates still pull it. Declared but unreferenced in 2 crates. |
| 10 | `num_cpus` | 0 | 2 / 1 | 1 / 105.7M | Swap | `std::thread::available_parallelism` replaces 2 calls. 2 other crates still pull it. |
| 10 | `once_cell` | 0 | 7 / 7 | 2 / 354M | Swap | `std::sync::OnceLock` and `LazyLock` replace it in 7 files with no new code. 33 crates still pull it, so this is cleanup only. Declared but unreferenced in 3 crates. |
| 10 | `relay-protocol-derive` | 0 | 0 / 0 | — | Delete | Declared in temps-error-tracking and never referenced there. Still locked through relay-event-schema until that goes. |
| 10 | `serde_derive` | 0 | 1 / 1 | 2 / 386.7M | Delete | Declaration only: `serde` with the `derive` feature already provides it. Stays in tree regardless. |
| 10 | `tiktoken-rs` | 0 | — | — | Delete | Optional, behind a feature no workspace crate enables, and unreferenced in source. |
| 10 | `validator` | 0 | — | — | Delete | Optional, behind a feature no workspace crate enables, and unreferenced in source. |
| 9 | `axum-test` (test) | 14 | 2 / 2 | 1 / 5M | Swap | Test only. `tower::ServiceExt::oneshot` covers it, tower is already everywhere. Used in 2 files, 2 more crates declare it unused. |
| 9 | `wasm-bindgen-test` (test) | 6 | 0 / 0 | 4 / 10.6M | Delete | Dev dependency of temps-captcha-wasm with no textual reference. Confirm with a wasm test build first. |
| 9 | `prost-build` | 4 | 1 / 1 | 2 / 93.3M | Build | Build-time only, 1 `build.rs` in temps-otel. Commit the generated OTLP code and regenerate on demand. Removes 4 crates and the `protoc` CI step. |
| 9 | `env_logger` (test) | 3 | 1 / 1 | 5 / 93.4M | Swap | Test only, 1 call. tracing-subscriber is already there. Removes 3 crates. Declared but unreferenced in 2 crates. |
| 9 | `axum-extra` | 2 | 1 / 1 | 4 / 15M | Build | One use: a `Query` extractor that accepts repeated keys. Small extractor on `url::form_urlencoded`. Removes 2 crates. |
| 9 | `axum-macros` | 1 | 6 / 1 | 4 / 33.7M | Delete | Declared in 12 crates; only 1 file uses it. Removes 1 crate if that use goes too. |
| 9 | `downcast-rs` | 1 | 1 / 1 | 1 / 56.8M | Build | One use. An `as_any()` trait method replaces it. |
| 9 | `gethostname` | 1 | 3 / 3 | 1 / 43.9M | Swap | 3 calls. `nix::unistd::gethostname` covers the Linux and macOS targets. |
| 9 | `maplit` (test) | 1 | 24 / 6 | 2 / 25.2M | Swap | Test only. `HashMap::from([...])` is in std. |
| 9 | `rustls-pemfile` | 1 | 44 / 15 | 3 / 80.4M | Swap | Unmaintained (RUSTSEC-2025-0134, currently ignored). `rustls-pki-types` `PemObject` replaces it and is already in tree. 15 files. Already tracked in `dependency-scan.yml`. |
| 9 | `temp-dir` (test) | 1 | 2 / 2 | 1 / 2.2M | Swap | Test only, 2 files. tempfile is already used everywhere. |
| 9 | `base32` | 0 | 12 / 5 | 1 / 9.2M | Keep | Tiny, but totp-rs pulls it. |
| 9 | `base64` (3 versions) | 0 | 215 / 81 | 2 / 468.1M | Keep | Buildable in minutes, but 34 other crates pull it. Nothing leaves the tree. Declared but unreferenced in 3 crates. |
| 9 | `colored` | 0 | 21 / 19 | 3 / 51.8M | Build | ANSI codes in 19 CLI files. Leaves the tree only once mockito goes. |
| 9 | `debugid` | 0 | 0 / 0 | 1 / 35.4M | Keep | Declared but unreferenced in temps-error-tracking. Stays through sourcemap and minidump. |
| 9 | `ipnetwork` | 0 | 6 / 5 | 1 / 21M | Swap | Duplicates ipnet, 5 files. Other crates still pull it. |
| 9 | `notify` | 0 | 0 / 0 | 4 / 52.5M | Delete | Declared in temps-logs and unreferenced. Stays through 1 other crate. |
| 9 | `percent-encoding` | 0 | 6 / 4 | 3 / 257.1M | Keep | Tiny, but 25 crates pull it. |
| 9 | `tokenizers` | 0 | — | — | Delete | Optional, behind a feature no workspace crate enables. Delete it with its gated code. It is the last Cargo.lock path to `paste` once `image` is slimmed. |
| 9 | `tokio-test` (test) | 0 | 4 / 4 | 2 / 20.8M | Delete | Test only, 4 references, declared in 21 crates and mostly unused. |
| 9 | `urlencoding` | 0 | 220 / 47 | 3 / 75.7M | Swap | percent-encoding (already in tree) covers it. 220 references. Stays locked until aws-smithy-query and cloudflare go. Declared but unreferenced in 2 crates. |
| 8 | `mockall` (test) | 8 | 4 / 4 | 1 / 33.3M | Swap | Test only, 9 `mock!`/`automock` uses in 4 files. Hand-written fakes on the traits replace it. Declared but unreferenced in 3 crates. |
| 8 | `cron` | 6 | 5 / 3 | 1 / 9.8M | Swap | Two cron parsers are in the tree. Moving these 3 crates to croner removes 6 crates. Field-count semantics differ, so existing stored schedules need tests. |
| 8 | `csv` | 2 | 1 / 1 | 1 / 61.5M | Build | One use in revenue import. An RFC 4180 reader with quote and BOM handling is small and testable. Input is an uploaded file. Removes 2 crates. |
| 8 | `include_dir` | 2 | 9 / 9 | 1 / 20.8M | Build | Proc macro in 9 places. A `build.rs` generating `include_bytes!` tables removes a build-time macro. Removes 2 crates. |
| 8 | `picomatch-rs` | 2 | 1 / 1 | 1 / 7.9K | Swap | One call, version 0.1.0. globset is already in the tree. pnpm negation and `**` behaviour need tests. |
| 8 | `slug` | 2 | 1 / 1 | 1 / 8.8M | Build | Used by temps-environments; declared unused in temps-projects. Hand-written ASCII slugs replace it and deunicode. Output must stay stable for non-ASCII names. |
| 8 | `if-addrs` | 1 | 3 / 2 | 1 / 8.8M | Swap | 3 calls. `nix::ifaddrs` is already in the tree; needs the `net` feature. |
| 8 | `crc32fast` | 0 | 5 / 2 | 1 / 218.7M | Keep | CRC32 is small, but 6 crates pull it. |
| 8 | `dirs` | 0 | 22 / 18 | 1 / 102.3M | Build | `$HOME` and XDG logic for 2 targets, 18 files. Leaves little until its other user goes. |
| 8 | `ipnet` | 0 | 89 / 33 | 1 / 191.8M | Keep | CIDR math is simple, but 7 crates pull it. Keep as the single CIDR crate. |
| 8 | `log` | 0 | 4 / 1 | 1 / 360.6M | Keep | Facade only, but 60 crates pull it. Remove the unused declarations, not the crate. |
| 8 | `pem` (2 versions) | 0 | 5 / 3 | 1 / 103.6M | Keep | Base64 with headers, but 2 crates pull it. |

### Score 5 to 7

| Score | Library | Removes | Refs / files | Owners / 90-day downloads | Action | Why |
|---|---|---|---|---|---|---|
| 7 | `indicatif` | 3 | 4 / 4 | 2 / 54.6M | Build | CLI progress bars in 4 files. A small ANSI renderer does it. Removes 3 crates. |
| 7 | `cloudflare` (2 versions) | 2 | 11 / 3 | 3 / 85.6K | Build | Two copies, one crates.io and one git rev. About 10 DNS REST calls on reqwest. At minimum unify the two; building it also drops a git dependency. |
| 7 | `mockito` (test) | 2 | 120 / 10 | 1 / 10M | Swap | Test only, 120 references in 10 files. Duplicates wiremock. Removes 2 crates, and the only non-CLI user of colored. |
| 7 | `serial_test` (test) | 2 | 171 / 25 | 1 / 37.6M | Keep | Test only. A static mutex replaces `#[serial]`, but 171 references for 2 crates is poor value. |
| 7 | `utoipa-swagger-ui` | 2 | 2 / 2 | 1 / 7.2M | Keep | Serves vendored Swagger assets (the `vendored` feature is on, so no build-time download). A static handler is feasible; low gain. Declared unused in temps-core. |
| 7 | `croner` | 1 | 1 / 1 | 1 / 5.2M | Keep | Single call in temps-monitoring. Keep this one and move `cron` users here. |
| 7 | `tokio-socks` | 1 | 4 / 1 | 1 / 4.1M | Keep | SOCKS5 handshake is short. 1 file and 1 crate removed, low value. |
| 7 | `anyhow` | 0 | 2546 / 223 | 1 / 269.1M | Keep | Could be typed errors as CLAUDE.md prefers, but that is a code-quality change. Nothing leaves the tree. Declared but unreferenced in 5 crates. |
| 7 | `mime_guess` | 0 | 5 / 5 | 1 / 85.9M | Keep | A static table covers it, but 3 other crates pull it. Declared but unreferenced in 1 crate. |
| 7 | `thiserror` (2 versions) | 0 | 438 / 392 | 1 / 483.5M | Keep | Could be hand-written Display impls, but it is required style in CLAUDE.md and 70 crates pull it. Declared but unreferenced in 7 crates. |
| 7 | `uuid` | 0 | 582 / 238 | 2 / 238M | Keep | v4 and v7 are short, but 22 crates pull it. Declared but unreferenced in 7 crates. |
| 7 | `walkdir` | 0 | 7 / 4 | 1 / 189M | Keep | Short with `std::fs`, but symlink loops matter and 3 crates pull it. |
| 6 | `relay-event-schema` | 26 | 6 / 5 | — | Build | Sentry event schema from a git dependency. Own serde structs for the fields `mapper.rs` actually reads. Input is untrusted SDK JSON, so it needs golden fixtures. Removes 31 crates with the rest of the relay group. |
| 6 | `octocrab` | 10 | 31 / 6 | 1 / 3M | Build later | Typed reqwest client for the GitHub REST calls used. About 79 references in 6 files. Needs pagination and rate-limit handling; App JWT signing stays on jsonwebtoken. |
| 6 | `headless_chrome` | 7 | 2 / 1 | 3 / 732.7K | Build later | Only navigate, evaluate and capture are used, in 1 file. A small CDP client over tokio-tungstenite would do. Also drops zip 8 and its 3 ignored advisories (RUSTSEC-2026-0367/0368/0369). |
| 6 | `testcontainers` | 7 | 111 / 81 | 3 / 17.3M | Build later | Mostly test. A small helper on bollard (already in tree) can start and stop containers. Touches 81 files, so it is mechanical but wide. Declared but unreferenced in 3 crates. |
| 6 | `htmd` | 5 | 5 / 1 | 1 / 7.3M | Build later | HTML to Markdown in 1 file. Feasible on top of a kept HTML parser. |
| 6 | `postcard` | 4 | 9 / 3 | 1 / 36.2M | Build later | Shield snapshots only, 3 files. A hand-written codec is feasible, but the on-disk format and untrusted decode need care. |
| 6 | `tokio-postgres-rustls` | 4 | 1 / 1 | 1 / 6.9M | Build later | One reference. About 150 lines of glue on tokio-rustls. TLS-adjacent, so needs real server tests. Removes 4 crates. |
| 6 | `sourcemap` | 3 | 4 / 3 | 1 / 4.5M | Build later | VLQ plus JSON, 3 files, but indexed maps and debug IDs add cases, and input is uploaded by clients. |
| 6 | `wiremock` (test) | 3 | 57 / 18 | 1 / 32.2M | Keep | Test only. Keep this one as the single HTTP mock and move mockito's tests onto it. Declared but unreferenced in 1 crate. |
| 6 | `aws-sdk-sesv2` | 1 | 27 / 2 | 2 / 1.4M | Build later | One signed POST to send mail. Only useful if SigV4 is owned for S3 as well. |
| 6 | `aws-sdk-sns` | 1 | 5 / 1 | 2 / 1.2M | Build later | Small API surface. Same condition as sesv2. |
| 6 | `maxminddb` | 1 | 12 / 2 | 1 / 6.2M | Keep | MMDB is a small format. Removes only 1 crate. |
| 6 | `totp-rs` | 1 | 7 / 5 | 1 / 3.3M | Keep | RFC 6238 is short with test vectors, but it guards MFA and removes only 2 crates with base32. |
| 6 | `async-stream` | 0 | 25 / 13 | 2 / 81.8M | Keep | `futures::stream::unfold` replaces most uses, but 2 other crates pull it. Declared but unreferenced in 1 crate. |
| 6 | `glob` | 0 | 3 / 1 | 1 / 159.4M | Keep | Short, but sea-orm-cli pulls it. |
| 6 | `md5` | 0 | 3 / 2 | 1 / 45.5M | Keep | Non-security checksum, simple with test vectors. russh still pulls it. |
| 6 | `parking_lot` | 0 | 20 / 13 | 2 / 264.2M | Keep | `std::sync::Mutex` could replace it in 13 files, but 17 crates pull it. |
| 6 | `relay-protocol` | 0 | 6 / 5 | — | Build | Goes with relay-event-schema: `Annotated` and `FromValue` wrappers used in 5 files. |
| 6 | `semver` | 0 | 11 / 4 | 1 / 262.9M | Keep | Spec is short, 5 crates pull it. |
| 6 | `serde_path_to_error` | 0 | 2 / 1 | 1 / 111.2M | Keep | Small wrapper around serde, 5 crates pull it. |
| 6 | `xxhash-rust` | 0 | 1 / 1 | 1 / 43.9M | Keep | Spec is short, redis pulls it anyway. |
| 5 | `jsonschema` | 12 | 1 / 1 | 1 / 36.5M | Build later | One call site validating LLM structured output. Full JSON Schema is large; a subset is feasible but narrows behaviour, so it needs a decision on supported keywords. Also removes fancy-regex. |
| 5 | `qrcode` | 1 | 1 / 1 | 1 / 7M | Keep | Reed-Solomon and masking are well specified, 1 call for MFA enrolment. Removes only 1 crate. |
| 5 | `sysinfo` | 1 | 12 / 9 | 1 / 58.5M | Keep | `/proc` on Linux is easy, macOS is not. Removes only 1 crate. |
| 5 | `woothee` | 1 | 5 / 5 | 1 / 1.9M | Keep | User-agent rules. The code is easy, keeping rules current is the work. Removes only 1 crate. |
| 5 | `async-trait` | 0 | 1147 / 973 | 1 / 192.2M | Keep | Could be manual `Box::pin`, but dyn-safe async traits still need it in 973 files. 20 crates pull it. Declared but unreferenced in 6 crates. |
| 5 | `cookie` | 0 | 37 / 7 | 1 / 67.4M | Keep | Parsing is easy, the encrypted jar is not. |
| 5 | `tempfile` | 0 | 1118 / 195 | 1 / 234M | Keep | Secure creation is subtle and 6 other crates pull it. Declared but unreferenced in 1 crate. |
| 5 | `tokio-stream` | 0 | 34 / 14 | 3 / 141.9M | Keep | Thin wrappers, 7 crates pull it. Declared but unreferenced in 2 crates. |

### Score 1 to 4

| Score | Library | Removes | Refs / files | Owners / 90-day downloads | Action | Why |
|---|---|---|---|---|---|---|
| 4 | `clickhouse` | 8 | 131 / 32 | 2 / 5.6M | Keep | HTTP plus RowBinary format is feasible, but it is on the analytics write path across 32 files. Declared but unreferenced in 2 crates. |
| 4 | `aws-sdk-s3` | 5 | 334 / 38 | 2 / 27.9M | Build later | SigV4 and multipart are well specified, but backups depend on this across 38 files and must work with S3-compatible stores. Removes 33 crates with the AWS group and 2 advisory exceptions. |
| 4 | `aws-config` | 4 | 14 / 5 | 2 / 42.8M | Build later | Credential chain. Replaceable if the AWS group goes, otherwise keep. Declared but unreferenced in 1 crate. |
| 4 | `opentelemetry-otlp` | 4 | 6 / 1 | 3 / 52.8M | Keep | Exporter in 1 file. Feasible with generated protos and an HTTP POST, but gain is small unless the whole OTel group goes (7 crates). |
| 4 | `redis` | 4 | 80 / 12 | 2 / 33.7M | Keep | RESP is easy; TLS, pooling and 80 references across 12 files are not worth it. |
| 4 | `pulldown-cmark` | 2 | 4 / 1 | 3 / 64.9M | Keep | CommonMark has a large spec. 1 file, 2 crates. |
| 4 | `tower-http` (2 versions) | 2 | 7 / 3 | 2 / 200.5M | Keep | CORS, tracing and compression middleware. Declared unused in 5 crates, still needed in others. |
| 4 | `aws-credential-types` | 0 | 2 / 2 | 1 / 47.1M | Build later | Part of the AWS group. |
| 4 | `aws-smithy-http-client` | 0 | 4 / 2 | 1 / 43.6M | Build later | Part of the AWS group. |
| 4 | `aws-smithy-runtime` | 0 | 0 / 0 | 1 / 45.8M | Build later | Part of the AWS group. Declared for features in temps-providers. |
| 4 | `aws-smithy-runtime-api` | 0 | 2 / 1 | 1 / 48.2M | Build later | Part of the AWS group. |
| 4 | `aws-smithy-types` | 0 | 0 / 0 | 3 / 48.6M | Build later | Part of the AWS group. Declared unused in temps-query-s3. |
| 4 | `aws-types` | 0 | 0 / 0 | 2 / 45.7M | Build later | Part of the AWS group. Declared unused in temps-query-s3. |
| 4 | `globset` | 0 | 2 / 1 | 1 / 68.6M | Keep | Glob compilation. Keep as the target for picomatch-rs. |
| 4 | `http-body-util` | 0 | 27 / 19 | 1 / 194.6M | Keep | Thin helpers, 19 crates pull it. Declared but unreferenced in 2 crates. |
| 4 | `rustls-native-certs` | 0 | 1 / 1 | 3 / 170M | Keep | Loads OS trust stores per platform. |
| 4 | `schemars` (2 versions) | 0 | 37 / 14 | 1 / 228.9M | Keep | JSON Schema derive. Declared but unreferenced in 1 crate. |
| 4 | `sha1_smol` | 0 | 4 / 2 | 1 / 65.7M | Keep | Non-cryptographic checksums in source maps. redis and uuid pull it. |
| 3 | `scraper` | 7 | 20 / 5 | 3 / 10.4M | Keep | HTML5 parsing plus CSS selectors. Do not hand-write an HTML parser. |
| 3 | `lopdf` | 5 | 47 / 1 | 1 / 13.6M | Keep | PDF parsing of untrusted files. |
| 3 | `git2` | 4 | 54 / 3 | 1 / 18.1M | Keep | libgit2 bindings, 3 files. The realistic alternative is shelling out to `git`, not writing one. |
| 3 | `lettre` | 4 | 11 / 2 | 2 / 8.5M | Keep | SMTP with TLS and MIME encoding, header injection risk. |
| 3 | `lol_html` | 4 | 2 / 1 | 2 / 2.4M | Keep | Streaming HTML rewriter, 1 file. Parser edge cases on untrusted HTML. |
| 3 | `tokio-tungstenite` (2 versions) | 2 | 58 / 9 | 2 / 104.6M | Keep | WebSocket framing on untrusted peers. |
| 3 | `dashmap` (2 versions) | 1 | 10 / 6 | 1 / 100.4M | Keep | Lock-sharded concurrent map with unsafe internals. Two versions in tree. |
| 3 | `instant-acme` | 1 | 9 / 3 | 2 / 1.5M | Keep | ACME protocol and signing. |
| 3 | `quick-xml` | 1 | 8 / 1 | 3 / 158.7M | Keep | XML parsing with entity and malformed-input edge cases. |
| 3 | `tracing-opentelemetry` | 1 | 3 / 1 | 4 / 58.4M | Keep | Bridge between two evolving APIs. |
| 3 | `unicode-casefold` | 1 | 1 / 1 | 1 / 838.6K | Keep | Used for portable path collision checks in sandbox workspaces. Unicode tables, and security relevant. |
| 3 | `bollard` | 0 | 2074 / 173 | 1 / 20.9M | Keep | Docker API client across 173 files. Declared but unreferenced in 1 crate. |
| 3 | `clap` | 0 | 63 / 32 | 3 / 273.8M | Keep | CLI parser with derive, 32 files. |
| 3 | `futures` | 0 | 363 / 146 | 1 / 207.7M | Keep | Same as futures-util. Declared but unreferenced in 7 crates. |
| 3 | `futures-util` | 0 | 43 / 26 | 1 / 290.5M | Keep | Combinators, 39 crates pull it. Declared but unreferenced in 6 crates. |
| 3 | `hyper-rustls` | 0 | 2 / 1 | 3 / 207.9M | Keep | TLS glue, 6 crates pull it. |
| 3 | `memchr` | 0 | 3 / 2 | 1 / 436.2M | Keep | SIMD search, 31 crates pull it. Declared but unreferenced in 1 crate. |
| 3 | `moka` | 0 | 29 / 17 | 1 / 45.8M | Keep | Concurrent cache with eviction. |
| 3 | `nix` (3 versions) | 0 | 30 / 11 | 2 / 228.7M | Keep | Safe wrappers on libc. Three versions in tree. Useful as the home for if-addrs and gethostname. |
| 3 | `rand` (3 versions) | 0 | 169 / 59 | 2 / 581.1M | Keep | Randomness for secrets must stay on a vetted CSPRNG. Three versions in tree. Declared but unreferenced in 1 crate. |
| 3 | `rustls-pki-types` | 0 | 10 / 2 | 2 / 210.9M | Keep | Shared key and certificate types. |
| 3 | `tar` | 0 | 269 / 39 | 1 / 72.1M | Keep | Extraction has path-traversal history. Backups depend on it. Declared but unreferenced in 2 crates. |
| 3 | `time` | 0 | 48 / 27 | 1 / 227.1M | Keep | 24 crates pull it. Declared but unreferenced in 1 crate. |
| 3 | `tokio-util` | 0 | 69 / 33 | 3 / 218.1M | Keep | Codecs and cancellation tokens, 17 crates pull it. Declared but unreferenced in 3 crates. |
| 3 | `toml` | 0 | 36 / 4 | 2 / 290.8M | Keep | Parser, 3 crates pull it. |
| 3 | `tonic` | 0 | — | — | Keep | gRPC. Reached through bollard and the local `opentelemetry-proto` patch crate, not used directly. |
| 3 | `tonic-prost` | 0 | — | — | Keep | gRPC codec. Same path as tonic. |
| 3 | `tracing-subscriber` | 0 | 43 / 11 | 3 / 179.6M | Keep | Log formatting and filtering. |
| 3 | `url` | 0 | 189 / 79 | 5 / 240.3M | Keep | WHATWG parsing. Mismatches here cause SSRF. Declared but unreferenced in 2 crates. |
| 3 | `utoipa` | 0 | 2255 / 470 | 1 / 20.7M | Keep | OpenAPI derive across 470 files. |
| 3 | `zip` (2 versions) | 0 | 46 / 9 | 2 / 96.5M | Keep | Archive extraction on uploaded files. Advisory history shows the risk. |
| 2 | `defguard_wireguard_rs` | 9 | 10 / 2 | 2 / 129.5K | Keep | WireGuard kernel and userspace control. Security infrastructure. |
| 2 | `openidconnect` | 7 | 15 / 3 | 1 / 5.3M | Keep | Token validation for sign-in. Getting it wrong is an auth bypass. |
| 2 | `rtnetlink` | 2 | 15 / 5 | 5 / 5.2M | Keep | Linux netlink control. |
| 2 | `yaml_serde` | 2 | 334 / 20 | 1 / 4.1M | Keep | YAML parsing of user compose and config files. Notoriously hard to get right. |
| 2 | `chrono-tz` | 1 | 1 / 1 | 2 / 50.5M | Keep | IANA timezone data that must stay current. |
| 2 | `hickory-server` | 1 | 6 / 3 | 1 / 3M | Keep | DNS server parsing untrusted packets. |
| 2 | `jsonwebtoken` (2 versions) | 1 | 27 / 6 | 1 / 66.9M | Keep | JWT verification. Algorithm-confusion bugs live here. |
| 2 | `webpki-root-certs` | 1 | 2 / 2 | 4 / 62.3M | Keep | Trust root data must be maintained by someone accountable. |
| 2 | `arc-swap` | 0 | 54 / 40 | 2 / 106.2M | Keep | Lock-free unsafe code on the proxy hot path. |
| 2 | `bson` (2 versions) | 0 | 11 / 1 | 1 / 7.4M | Keep | Tied to the mongodb driver. |
| 2 | `bytes` | 0 | 172 / 81 | 3 / 309M | Keep | Buffer foundation with unsafe internals. Declared but unreferenced in 2 crates. |
| 2 | `chrono` | 0 | 4120 / 753 | 2 / 222.2M | Keep | Date arithmetic and formatting, 753 files. Declared but unreferenced in 9 crates. |
| 2 | `flate2` | 0 | 130 / 33 | 1 / 210M | Keep | Compression wrapper, 10 crates pull it. Declared but unreferenced in 3 crates. |
| 2 | `getrandom` (3 versions) | 0 | 0 / 0 | 2 / 741.3M | Keep | OS randomness. Declared as a wasm feature pin in temps-captcha-wasm. |
| 2 | `hickory-proto` | 0 | 12 / 4 | 1 / 32.1M | Keep | DNS wire format. |
| 2 | `hickory-resolver` | 0 | 44 / 11 | 1 / 30.5M | Keep | DNS resolver with DNSSEC and transport logic. |
| 2 | `hkdf` (2 versions) | 0 | 3 / 3 | 2 / 106.3M | Keep | Short on top of HMAC, but crypto stays on audited code. Declared but unreferenced in 1 crate. |
| 2 | `http` (2 versions) | 0 | 245 / 190 | 2 / 305M | Keep | Shared type vocabulary with hyper and axum. Declared but unreferenced in 2 crates. |
| 2 | `hyper-util` | 0 | 29 / 12 | 1 / 194.6M | Keep | Connection management, 15 crates pull it. Declared but unreferenced in 2 crates. |
| 2 | `image` | 0 | 19 / 4 | 4 / 73M | Slim | Do not build, but stop pulling AVIF and the other codecs. temps-auth only writes a PNG. Measured: 1 line removes 35 crates. |
| 2 | `netlink-packet-core` | 0 | 0 / 0 | 4 / 11.4M | Keep | Version pin for netlink. |
| 2 | `netlink-packet-route` | 0 | 7 / 3 | 5 / 11.3M | Keep | Netlink message definitions. |
| 2 | `opentelemetry` | 0 | 8 / 1 | 3 / 80.5M | Keep | OTel API. |
| 2 | `opentelemetry_sdk` | 0 | 7 / 1 | 3 / 74.1M | Keep | Batching and export pipeline. |
| 2 | `prost` (2 versions) | 0 | 9 / 5 | 2 / 162.1M | Keep | Protobuf runtime. |
| 2 | `rcgen` | 0 | 36 / 11 | 2 / 50.1M | Keep | X.509 generation. |
| 2 | `rustix` | 0 | 14 / 2 | 1 / 334M | Keep | Syscall bindings. |
| 2 | `sha1` (2 versions) | 0 | 3 / 2 | 3 / 201.2M | Keep | Crypto, 10 crates pull it. |
| 2 | `tokio-postgres` | 0 | 84 / 12 | 2 / 24.1M | Keep | Postgres driver. |
| 2 | `tokio-rustls` | 0 | 15 / 9 | 3 / 222M | Keep | TLS on async streams. |
| 2 | `tower` | 0 | 78 / 54 | 3 / 221.6M | Keep | Middleware foundation. Declared but unreferenced in 4 crates. |
| 2 | `tracing` | 0 | 2651 / 776 | 3 / 249.8M | Keep | Logging foundation, 54 crates depend on it. Declared but unreferenced in 1 crate. |
| 2 | `unicode-normalization` | 0 | 1 / 1 | 5 / 135.5M | Keep | Unicode tables. |
| 2 | `webpki-roots` (2 versions) | 0 | 1 / 1 | 3 / 238.2M | Keep | Trust root data. Declared but unreferenced in 1 crate. |
| 2 | `x509-parser` | 0 | 17 / 9 | 1 / 62.9M | Keep | ASN.1 parsing of untrusted certificates. |
| 2 | `zeroize` | 0 | 3 / 3 | 2 / 234.9M | Keep | Volatile wipes that optimisers must not remove. |
| 1 | `microsandbox` | 95 | 3 / 2 | 2 / 27.9K | Isolate | microVM SDK with its own VMM. Cannot be rebuilt, but it is called from only 2 files and holds up the second sea-orm and sqlx majors. Move it behind a feature flag or a separate binary (ADR-050). |
| 1 | `russh` | 25 | 21 / 1 | 1 / 4.2M | Keep | SSH protocol and crypto. Never hand-write. |
| 1 | `mongodb` | 14 | 34 / 9 | 1 / 5.1M | Keep | Wire protocol, SCRAM auth, topology handling. |
| 1 | `sea-orm-migration` (2 versions) | 3 | 393 / 387 | 2 / 3.7M | Keep | Migration framework, 387 files. Declared but unreferenced in 5 crates. |
| 1 | `tikv-jemallocator` | 2 | 2 / 1 | 1 / 25.2M | Keep | Allocator. The memory targets in CLAUDE.md lean on it. |
| 1 | `pingora` | 1 | 31 / 6 | 5 / 1.1M | Keep | The proxy runtime. |
| 1 | `aes-gcm` | 0 | 3 / 3 | 2 / 66.4M | Keep | Crypto. Declared but unreferenced in 1 crate. |
| 1 | `argon2` | 0 | 50 / 12 | 2 / 27.7M | Keep | Password hashing. Never hand-write. |
| 1 | `axum` | 0 | 2203 / 331 | 4 / 148.9M | Keep | HTTP framework, 331 files. Declared but unreferenced in 1 crate. |
| 1 | `ed25519-dalek` (2 versions) | 0 | 11 / 7 | 2 / 77.9M | Keep | Crypto. |
| 1 | `hmac` (2 versions) | 0 | 36 / 25 | 2 / 209.2M | Keep | Crypto. |
| 1 | `hyper` | 0 | 67 / 14 | 2 / 261.8M | Keep | HTTP implementation. Request smuggling lives here. Declared but unreferenced in 4 crates. |
| 1 | `js-sys` | 0 | 2 / 1 | 4 / 153M | Keep | wasm-bindgen ecosystem. |
| 1 | `libc` | 0 | 238 / 29 | 1 / 452.5M | Keep | FFI bindings. |
| 1 | `openssl` | 0 | 0 / 0 | 2 / 81.9M | Keep | Declared for Pingora's TLS feature path. |
| 1 | `p256` (2 versions) (test) | 0 | 4 / 1 | 2 / 70.3M | Keep | Crypto. |
| 1 | `pingora-core` | 0 | 41 / 12 | 5 / 1.3M | Keep | Proxy runtime. |
| 1 | `pingora-http` | 0 | 20 / 3 | 5 / 1.3M | Keep | Proxy runtime. |
| 1 | `pingora-load-balancing` | 0 | 0 / 0 | 5 / 978.5K | Keep | Proxy runtime. Declared unreferenced in temps-proxy; check whether it is a feature requirement. |
| 1 | `pingora-openssl` | 0 | 11 / 2 | 5 / 520.3K | Keep | Proxy TLS. |
| 1 | `pingora-proxy` | 0 | 9 / 4 | 5 / 1.2M | Keep | Proxy runtime. |
| 1 | `regex` | 0 | 44 / 19 | 3 / 306.2M | Keep | Linear-time engine protects against ReDoS. |
| 1 | `reqwest` (2 versions) | 0 | 983 / 143 | 1 / 243.3M | Keep | HTTP client with TLS, redirects and proxies. Declared but unreferenced in 2 crates. |
| 1 | `rsa` (2 versions) | 0 | 28 / 6 | 2 / 66.7M | Keep | Crypto. Never hand-write. Migration away is tracked as an advisory exception, not a rewrite. |
| 1 | `rustls` | 0 | 241 / 24 | 3 / 261.6M | Keep | TLS. Never hand-write. |
| 1 | `sea-orm` (2 versions) | 0 | 4781 / 885 | 2 / 5.7M | Keep | ORM, 885 files. Declared but unreferenced in 1 crate. |
| 1 | `serde` | 0 | 1217 / 890 | 2 / 399M | Keep | Foundation of the ecosystem. Declared but unreferenced in 7 crates. |
| 1 | `serde_json` | 0 | 9160 / 868 | 2 / 399.9M | Keep | Parser performance and correctness on untrusted input. Declared but unreferenced in 1 crate. |
| 1 | `sha2` (2 versions) | 0 | 174 / 126 | 3 / 347.3M | Keep | Crypto with audited, accelerated code. Never hand-write. Declared but unreferenced in 2 crates. |
| 1 | `sqlx` (2 versions) | 0 | 199 / 22 | 2 / 53M | Keep | Database drivers. The separate 0.9 declaration in temps-proxy looks unused: see Delete tier. |
| 1 | `subtle` | 0 | 22 / 9 | 2 / 213.4M | Keep | Constant-time primitives. Never hand-write. |
| 1 | `tokio` | 0 | 13032 / 941 | 3 / 285.8M | Keep | Async runtime. Declared but unreferenced in 2 crates. |
| 1 | `wasm-bindgen` | 0 | 1 / 1 | 4 / 164.5M | Keep | Required for wasm targets. |
| 1 | `x25519-dalek` | 0 | 21 / 8 | 2 / 29M | Keep | Crypto. Never hand-write. |
| 1 | `zstd` (2 versions) | 0 | 21 / 4 | 1 / 110.3M | Keep | C library bindings. |

### First-party (not scored)

| Score | Library | Removes | Refs / files | Owners / 90-day downloads | Action | Why |
|---|---|---|---|---|---|---|
| n/a | `autopack-dockerfile` | 1 | 1 / 1 | — | First-party | gotempsh git dependency, pinned to tag v0.1.5 with the SHA in Cargo.lock. |
| n/a | `autopack-providers` | 1 | 12 / 2 | — | First-party | gotempsh git dependency. |
| n/a | `temps-agent-runtime` | 1 | 35 / 4 | — | First-party | gotempsh git dependency, pinned to a full rev. |
| n/a | `autopack-core` | 0 | 7 / 2 | — | First-party | gotempsh git dependency. |

## Scorecard: JavaScript

**Removes** here is an approximate count from the lockfile graph. It is the largest count across the lockfiles that list the package, and it ignores peer dependencies. Only the seven-package web removal above was measured by re-resolving.

### Web console (`web/` and `web/packages/*`)

| Score | Package | Removes | Kind | Action | Why |
|---|---|---|---|---|---|
| 10 | `@nivo/line` | ≈36 | runtime | Delete | Zero references in `web/`. Part of the measured 52-entry removal. |
| 10 | `rehype-highlight` | ≈6 | runtime | Delete | Zero references in `web/`. |
| 10 | `@radix-ui/react-hover-card` | ≈1 | runtime | Delete | Zero references in `web/`. |
| 10 | `cobe` | ≈1 | runtime | Delete | Zero references in `web/`. |
| 10 | `date-fns-tz` | ≈1 | runtime | Delete | Zero references in `web/`. |
| 10 | `ghostty-web` | ≈1 | runtime | Delete | Zero references in `web/`. |
| 10 | `highlight.js` | ≈1 | runtime | Delete | Zero references in `web/`. Goes with rehype-highlight and lowlight. |
| 10 | `clsx` | ≈0 | runtime | Build | 10 lines in `src/lib/utils.ts`, its only user. |
| 9 | `@radix-ui/react-separator` | ≈1 | runtime | Build | A styled `<div role=separator>`. 1 wrapper file. |
| 9 | `iso-3166` | ≈1 | runtime | Build | A static country table, 1 file. |
| 9 | `react-icons` | ≈1 | runtime | Build | 2 files. Vendor those icons, or move them to lucide-react. Second icon library. |
| 9 | `slugify` | ≈1 | runtime | Build | 1 file. `web/` already has 2 hand-written slugify functions to reuse. |
| 8 | `@radix-ui/react-label` | ≈1 | runtime | Keep | A styled `<label>`, but Radix primitives stay in tree. |
| 8 | `@radix-ui/react-progress` | ≈1 | runtime | Build | A div with ARIA values. 1 wrapper file. |
| 8 | `class-variance-authority` | ≈1 | runtime | Build | 6 files. A small variant-lookup function. |
| 8 | `nanoid` | ≈1 | runtime | Build | `customAlphabet` in 3 files. `crypto.getRandomValues` with rejection sampling; a naive modulo adds bias. |
| 8 | `next-themes` | ≈1 | runtime | Build | 5 files. About 80 lines of localStorage, a class on `<html>` and a media query. This is an SPA, so its SSR handling is unused. |
| 8 | `simple-icons` | ≈1 | runtime | Build | 2 files. Vendor the brand SVGs used, keeping attribution. |
| 8 | `tailwindcss-animate` | ≈1 | runtime | Build | A few keyframes and utilities, referenced from `tailwind.config.ts`. |
| 8 | `xterm-addon-fit` | ≈1 | runtime | Build | About 50 lines of sizing maths in 1 file. |
| 7 | `@monaco-editor/react` | ≈3 | runtime | Build | Thin loader in 3 files. |
| 7 | `@hookform/resolvers` | ≈1 | runtime | Keep | A small Zod adapter, but 102 files and low gain. |
| 7 | `@radix-ui/react-avatar` | ≈1 | runtime | Build | An image with a fallback. 1 wrapper file. |
| 6 | `@react-three/drei` | ≈39 | runtime | Build later | Helpers used by 1 file (`EarthGlobe.tsx`). Direct three.js code could replace it. |
| 6 | `@react-three/fiber` | ≈7 | runtime | Build later | Renderer for the same 1 file. |
| 6 | `ansi-to-html` | ≈2 | runtime | Keep | 5 files. Output goes into the DOM, so a rewrite must get escaping right. XSS risk. |
| 6 | `@hey-api/client-fetch` | ≈1 | runtime | Keep | Small fetch wrapper, tied to the generator. |
| 6 | `@radix-ui/react-collapsible` | ≈1 | runtime | Keep | Small, but shares Radix internals that stay anyway. |
| 6 | `fuse.js` | ≈1 | runtime | Keep | Fuzzy ranking in 3 files. No dependencies, so low risk to keep. |
| 6 | `lucide-react` | ≈1 | runtime | Keep | Icons in 680 files. Vendoring used SVGs is feasible and mechanical, but the package has no real logic to attack beyond a bad release. |
| 6 | `@radix-ui/react-slot` | ≈0 | runtime | Keep | Small, but 9 Radix packages need it. |
| 5 | `react-simple-maps` | ≈18 | runtime | Keep | 1 file. Could render SVG from d3-geo directly. |
| 5 | `cmdk` | ≈6 | runtime | Keep | Command palette, 1 wrapper, keyboard behaviour. |
| 5 | `@types/react-simple-maps` | ≈5 | dev | Keep | Types only. Goes if react-simple-maps goes. |
| 5 | `@tailwindcss/typography` | ≈4 | dev | Keep | CSS only. |
| 5 | `@happy-dom/global-registrator` | ≈1 | dev | Keep | Small adapter to happy-dom. |
| 5 | `sonner` | ≈1 | runtime | Keep | Toasts in 299 files. |
| 4 | `react-day-picker` | ≈2 | runtime | Keep | Calendar logic in 13 files. |
| 4 | `@radix-ui/react-checkbox` | ≈1 | runtime | Keep | Accessibility behaviour. |
| 4 | `@radix-ui/react-switch` | ≈1 | runtime | Keep | Accessibility behaviour. |
| 4 | `@radix-ui/react-tabs` | ≈1 | runtime | Keep | Accessibility behaviour. |
| 4 | `react-resizable-panels` | ≈1 | runtime | Keep | Pointer and layout logic in 4 files. |
| 4 | `date-fns` | ≈0 | runtime | Keep | 84 files. `Intl` covers formatting but not all the arithmetic. |
| 3 | `recharts` | ≈37 | runtime | Keep | Charts in 15 files. |
| 3 | `react-markdown` | ≈19 | runtime | Keep | Markdown rendering of AI output in 4 files. XSS-sensitive. |
| 3 | `remark-gfm` | ≈18 | runtime | Keep | GFM parsing extension. |
| 3 | `@testing-library/react` | ≈14 | dev | Keep | Test toolchain. |
| 3 | `@testing-library/dom` | ≈9 | dev | Keep | Peer dependency of @testing-library/react. |
| 3 | `@types/bun` | ≈4 | dev | Keep | Types only, no runtime code. |
| 3 | `rrweb-player` | ≈3 | runtime | Keep | Session replay player. |
| 3 | `@tanstack/react-virtual` | ≈2 | runtime | Keep | Virtualisation in 4 files. |
| 3 | `@radix-ui/react-alert-dialog` | ≈1 | runtime | Keep | Accessibility behaviour is the hard part. |
| 3 | `@radix-ui/react-popover` | ≈1 | runtime | Keep | Positioning and focus. |
| 3 | `@radix-ui/react-radio-group` | ≈1 | runtime | Keep | Keyboard behaviour. |
| 3 | `@radix-ui/react-scroll-area` | ≈1 | runtime | Keep | Custom scroll behaviour. |
| 3 | `@radix-ui/react-slider` | ≈1 | runtime | Keep | Pointer and keyboard behaviour. |
| 3 | `@radix-ui/react-tooltip` | ≈1 | runtime | Keep | Positioning and timing. |
| 3 | `eslint-plugin-react-refresh` | ≈1 | dev | Keep | Lint toolchain. |
| 3 | `eslint-plugin-unused-imports` | ≈1 | dev | Keep | Lint toolchain. |
| 3 | `react-hook-form` | ≈1 | runtime | Keep | Required by the console's form rules. |
| 3 | `tailwind-merge` | ≈1 | runtime | Keep | 1 file, but its class conflict table follows Tailwind releases. |
| 3 | `@radix-ui/react-dialog` | ≈0 | runtime | Keep | Focus trapping and accessibility. |
| 3 | `fflate` | ≈0 | runtime | Keep | Zip and compression on uploaded data. |
| 2 | `eslint-plugin-react` | ≈113 | dev | Keep | Lint toolchain. Largest dev subtree: a smaller linter would help more than a rewrite. |
| 2 | `@hey-api/openapi-ts` | ≈51 | dev | Keep | API client generator. |
| 2 | `eslint-plugin-react-hooks` | ≈31 | dev | Keep | Lint toolchain. |
| 2 | `shiki` | ≈21 | runtime | Keep | Grammar-based highlighting. |
| 2 | `eslint-plugin-prettier` | ≈6 | dev | Keep | Lint toolchain. |
| 2 | `@rsbuild/plugin-react` | ≈3 | dev | Keep | Bundler plugin. |
| 2 | `@types/react` | ≈3 | dev | Keep | Types only. |
| 2 | `@radix-ui/react-dropdown-menu` | ≈2 | runtime | Keep | Keyboard and focus behaviour. |
| 2 | `@tanstack/react-query` | ≈2 | runtime | Keep | Required by the console's state rules. |
| 2 | `@types/three` | ≈2 | runtime | Keep | Types only. |
| 2 | `@radix-ui/react-select` | ≈1 | runtime | Keep | Most complex primitive. |
| 2 | `@types/react-dom` | ≈1 | dev | Keep | Types only. |
| 2 | `eslint-config-prettier` | ≈1 | dev | Keep | Lint toolchain. |
| 2 | `zod` | ≈1 | runtime | Keep | Schema validation foundation. |
| 2 | `js-yaml` | ≈0 | runtime | Keep | One `load` call, but YAML parsing is not something to hand-write. |
| 1 | `eslint` | ≈56 | dev | Keep | Lint toolchain. |
| 1 | `@tailwindcss/postcss` | ≈39 | runtime | Keep | Build toolchain. |
| 1 | `@rsbuild/core` | ≈23 | dev | Keep | Bundler. |
| 1 | `@typescript/native` | ≈21 | dev | Keep | Toolchain alias. |
| 1 | `@typescript-eslint/eslint-plugin` | ≈5 | dev | Keep | Lint toolchain. |
| 1 | `monaco-editor` | ≈4 | runtime | Keep | Code editor. |
| 1 | `@playwright/test` | ≈3 | dev | Keep | Browser test toolchain. |
| 1 | `react-router` | ≈3 | runtime | Keep | Routing. |
| 1 | `react-dom` | ≈2 | runtime | Keep | Framework. |
| 1 | `@eslint/js` | ≈1 | dev | Keep | Lint toolchain. |
| 1 | `@typescript-eslint/parser` | ≈1 | dev | Keep | Lint toolchain. |
| 1 | `@typescript/old` | ≈1 | dev | Keep | Toolchain alias. |
| 1 | `axe-core` | ≈1 | dev | Keep | Accessibility rules engine. |
| 1 | `prettier` | ≈1 | dev | Keep | Formatter. |
| 1 | `react` | ≈1 | runtime | Keep | Framework. |
| 1 | `three` | ≈1 | runtime | Keep | 3D engine for one globe component. |
| 1 | `typescript` | ≈1 | runtime | Keep | Toolchain. |
| 1 | `typescript-eslint` | ≈1 | dev | Keep | Lint toolchain. |
| 1 | `xterm` | ≈1 | runtime | Keep | Terminal emulator. |
| 1 | `happy-dom` | ≈0 | dev | Keep | Test DOM. |
| 1 | `tailwindcss` | ≈0 | runtime | Keep | Build toolchain. |
| n/a | `@temps-sdk/react-analytics` | ≈3 | runtime | First-party | Published from this repository. |

### CLI, SDKs and tooling

| Score | Package | Removes | Kind | Action | Why |
|---|---|---|---|---|---|
| 10 | `chalk` | ≈0 | runtime | Swap | 16 CLI files. `node:util` `styleText` does this with no dependency (check the minimum runtime first). chalk was hit by the September 2025 npm account takeover. Other CLI packages may still pull it, so count removals after the swap. |
| 9 | `ora` | ≈12 | runtime | Build | Spinner in 2 files. |
| 9 | `@testing-library/react-hooks` | ≈2 | dev | Swap | Test only. `renderHook` is in @testing-library/react. |
| 9 | `ink-spinner` | ≈2 | runtime | Build | A short frame loop, 1 file. |
| 8 | `conf` | ≈19 | runtime | Build | JSON config file in 1 file. Needs atomic write and `0600` permissions since it may hold tokens. |
| 8 | `cli-table3` | ≈7 | runtime | Build | Table rendering in 1 file. |
| 8 | `@faker-js/faker` | ≈1 | runtime | Build | Seed script only. Small generators cover it. |
| 7 | `react-devtools-core` | ≈3 | dev | Keep | Dev only and optional for ink. |
| 6 | `maxmind` | ≈3 | runtime | Keep | Same format as maxminddb. telemetry-api only. |
| 6 | `jsonc-parser` | ≈1 | runtime | Keep | Small and has no dependencies. |
| 6 | `@anthropic-ai/sdk` | ≈0 | runtime | Keep | Acceptance tests only. |
| 5 | `commander` | ≈1 | runtime | Keep | 123 files. Too widely used in the CLI for the gain. |
| 5 | `ws` | ≈1 | runtime | Keep | 4 files. Runtime WebSocket clients may replace it, but server use does not. |
| 4 | `@inquirer/prompts` | ≈56 | runtime | Keep | Terminal prompts, 1 file. |
| 4 | `@inquirer/search` | ≈6 | runtime | Keep | Terminal prompts, 1 file. |
| 4 | `web-vitals` | ≈1 | runtime | Keep | Browser timing semantics change over time. |
| 3 | `@vitest/coverage-v8` | ≈16 | dev | Keep | Toolchain. |
| 3 | `@rrweb/packer` | ≈2 | runtime | Keep | Tied to rrweb's format. |
| 3 | `bun-types` | ≈1 | dev | Keep | Types only. |
| 3 | `yaml` | ≈0 | runtime | Keep | Acceptance tests only. YAML parser. |
| 2 | `jsdom` | ≈45 | dev | Keep | Test DOM. |
| 2 | `@vitejs/plugin-react` | ≈43 | dev | Keep | Toolchain. |
| 2 | `ink` | ≈33 | runtime | Keep | Terminal React renderer, 2 files. |
| 2 | `pg` | ≈6 | runtime | Keep | Postgres driver. |
| 2 | `@types/pg` | ≈1 | dev | Keep | Types only. |
| 2 | `@types/ws` | ≈1 | runtime | Keep | Types only. |
| 2 | `postgres` | ≈1 | runtime | Keep | Postgres driver, seed script. Could share `pg`. |
| 2 | `@types/node` | ≈0 | dev | Keep | Types only. |
| 1 | `promptfoo` | ≈827 | dev | Keep | LLM evaluation tool, dev only, large tree. |
| 1 | `vite` | ≈62 | dev | Keep | Toolchain. |
| 1 | `@tailwindcss/vite` | ≈39 | dev | Keep | Design-system toolchain. |
| 1 | `vitest` | ≈21 | dev | Keep | Toolchain. |
| 1 | `vue` | ≈17 | runtime | Keep | Framework peer for the SDK. |
| 1 | `svelte` | ≈12 | runtime | Keep | Framework peer for the SDK. |
| 1 | `rrweb` | ≈8 | runtime | Keep | Session recording. |

## Scorecard: Python

| Score | Package | Action | Why |
|---|---|---|---|
| 3 | `aiohttp` (`sdks/python`) | Keep | Async HTTP client and the SDK's only dependency. Keep it unless the SDK can go synchronous on the standard library. |

## Method and limits

**How it was measured:**

- **Rust tree:** `cargo metadata --locked` for `x86_64-unknown-linux-gnu`, including workspace dev dependencies. "Removes" recomputes what stays reachable with that dependency's workspace edges cut.
- **Measured cuts:** applied to a scratch copy and re-resolved with cargo or bun. Your worktree's dependencies were not changed.

**What could be wrong:**

- **Modelled counts are estimates.** They follow the current graph and do not account for feature changes when a crate leaves, so treat tiers 2 to 5 as approximate until each is re-resolved.
- **The unused list is a text search.** It can flag a dependency kept only to enable a feature, such as a version or feature pin. I excluded the ones I recognised, but each removal still needs to compile.
- **Other platforms differ.** macOS, musl and wasm trees were not measured. The microsandbox dependency is not linked on musl, so the container image gains less from tier 5.
- **Scope.** Direct dependencies are scored. About 793 transitive crates are not, because they can only be removed by dropping or replacing a parent. `examples/` and test fixtures are excluded.

**Sources:** [RustSec RUSTSEC-2024-0006](https://rustsec.org/advisories/RUSTSEC-2024-0006), [OSV record](https://osv.dev/vulnerability/RUSTSEC-2024-0006), [Cycode on the chalk and debug compromise](https://cycode.com/?p=10541) (a vendor report; I did not find an official npm advisory).
