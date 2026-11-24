# Shared Rust build cache with sccache and S3

An optional setup for developers and agent machines. It is not used by CI, which has
its own cache handling in `.github/workflows/rust-tests.yml`.

## What it helps and what it does not

`sccache` caches the output of each `rustc` invocation by a hash of its inputs.

- **Third-party dependencies:** these dominate a cold build (`Cargo.lock` lists
  about 1,400 packages). A second checkout, a second machine or a cleaned `target/`
  gets them from the cache instead of recompiling.
- **Workspace crates:** the `dev` profile in `Cargo.toml` sets `incremental = true`,
  and `sccache` does not cache incrementally compiled crates. Your own crates keep
  using normal incremental builds. If you set `CARGO_INCREMENTAL=0` to get cache hits
  for workspace crates too, single-crate edit-compile loops get slower, so leave it
  alone unless you are warming a cache.
- **Linking and test runs:** not cached. Linking time is unchanged.

Measure before you adopt it: run `cargo build --timings` on a cold `target/`, once
without and once with the cache, and compare. We have not benchmarked it on this
workspace.

## Local disk cache (no S3)

```bash
cargo install sccache --locked     # or: brew install sccache
export RUSTC_WRAPPER=sccache
sccache --show-stats
```

Add the `export` to your shell profile. Do not put `RUSTC_WRAPPER` in a committed
`.cargo/config.toml`: contributors without `sccache` installed would fail to build.

## Shared cache in S3

One bucket can be shared by every machine that builds this repo (for example a
Linux build host and several Macs). Use a bucket reachable only from your own
network, and give each machine credentials that can read and write that bucket
only.

```bash
export RUSTC_WRAPPER=sccache
export SCCACHE_BUCKET=temps-sccache
export SCCACHE_REGION=us-east-1
export SCCACHE_S3_KEY_PREFIX=temps/rust
# For an S3-compatible server (MinIO, RustFS, Garage), also set:
export SCCACHE_ENDPOINT=https://s3.internal.example.com
# Credentials come from the usual AWS variables or a profile:
export AWS_ACCESS_KEY_ID=...
export AWS_SECRET_ACCESS_KEY=...
```

Check that it is active:

```bash
sccache --stop-server >/dev/null 2>&1 || true   # fails harmlessly if no server is running
sccache --start-server
sccache --show-stats          # "Cache location" should name S3 and the bucket
```

Never commit credentials. Keep them in your shell profile or a secrets manager.

The `sccache` server reads its environment once, when it starts. After you change
any `SCCACHE_*` or `AWS_*` variable, run `sccache --stop-server` and then
`sccache --start-server` so the new values take effect. If no server is running,
a build starts one on demand, but a build never reloads the settings of a server
that is already running.

If the bucket is unreachable, check `sccache --show-stats` and your network first:
we have not tested how the installed `sccache` version behaves in that case, and
you can always unset `RUSTC_WRAPPER` to build without it.

### Untrusted checkouts: read-only credentials

Build scripts (`build.rs`) and procedural macros run arbitrary code during the
build and can read every environment variable, including the AWS credentials. Do
not give write-capable credentials to builds of untrusted code, such as external
pull requests. Use a separate read-only principal (an IAM user or key whose policy
allows only `s3:GetObject` and `s3:ListBucket` on the bucket) and also set:

```bash
export SCCACHE_S3_RW_MODE=READ_ONLY
```

Set it before the first `sccache` command in that environment. If a server is
already running, stop it (`sccache --stop-server`) so the next start picks up the
read-only mode and the read-only credentials.

The mode alone does not protect the cache: a build that can read write-capable
credentials can use them directly, so the credentials themselves must be read-only.
Only trusted builds (your own branches, `main`) should hold the write key.

### Making hits work across machines

A cache entry is keyed by the compiler version, flags and the contents of the
inputs. Machines only share entries when these match:

- Use the same `rustc` version on every machine (compare `rustc --version`).
- Use the same target triple. A Linux build and a macOS build do not share entries.
- Use the same absolute checkout path on every machine that should share entries
  (for example `/workspace` in every sandbox or container). Rust builds embed
  absolute paths in the compiler arguments and in debug info, and these go into the
  cache key, so a different path is a miss. This is the setup we recommend.
- `SCCACHE_BASEDIRS` tells `sccache` to strip a base directory before computing
  keys, and it may help when paths differ. It is **unverified for Rust builds** here:
  an upstream report (mozilla/sccache#2652) says it first covered C compiler inputs
  while Cargo still passes absolute paths in `--extern` and `-L` arguments. We have
  not tested a version of `sccache` against this workspace, so do not rely on it
  without checking: build once, then build from a checkout at a different path and
  confirm the second build reports cache hits in `sccache --show-stats`.
- Do not add a per-machine `--remap-path-prefix` to `RUSTFLAGS` to work around
  paths. The flag differs on each machine, so it changes the cache key itself.

### Running agents in parallel

Give each agent its own `CARGO_TARGET_DIR` so builds do not contend on one
`target/` lock, and point all of them at the same `sccache` bucket.

For a dependency compiled by one agent to be a cache hit for the others, the agents
must build from the same absolute checkout path (see "Making hits work across
machines"). Run each agent in its own container or sandbox with the checkout mounted
at the same path, such as `/workspace`. Separate git worktrees on one host have
different absolute paths, so hits between them are not guaranteed; they depend on
path normalization that we have not verified for Rust builds.

### Housekeeping

- Add a lifecycle rule to the bucket to expire old objects (for example after
  30 days), so the cache does not grow without limit.
- Clear it by deleting the bucket prefix. There is no local state to invalidate.
