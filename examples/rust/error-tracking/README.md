# Rust Error Tracking Example (with source context)

A minimal Rust (Axum) app that reports errors to Temps error tracking and — once
you upload its source — shows the actual source code around each stack frame.

## What it demonstrates

- Wiring the `sentry` crate against the Sentry-compatible DSN Temps injects as
  `SENTRY_DSN`.
- The **correct release setup** for a compiled binary (see [ADR-033]): `release`
  is read from `SENTRY_RELEASE`, which Temps injects with the deployed commit
  SHA. This is what lets uploaded source line up with the frames.
- `attach_stacktrace: true`, so captured errors carry a backtrace.

## Endpoints

- `GET /` — hello
- `GET /health` — health check
- `GET /boom` — deliberately reports an error to Temps

## Running locally

```bash
SENTRY_DSN="<your-temps-dsn>" cargo run
# then: curl localhost:8080/boom
```

> Build for release with debug info so backtraces symbolicate:
> add `[profile.release] debug = true` (or `debug = "line-tables-only"`).

## Seeing source code in the stack trace

1. Deploy this app to Temps (git-based deploy).
2. In the project's **Settings → General → Error Tracking Source Context**,
   turn the toggle **on**.
3. Get the source to Temps, keyed by the release the app reports
   (`SENTRY_RELEASE` = the deployed commit SHA):
   - **Automatic** (git deploys): with the toggle on, Temps captures the source
     from the build automatically.
   - **Manual / CI:** upload it yourself, keyed by the same release:
     ```bash
     bunx @temps-sdk/cli errors source-files upload \
       --project-id <id> --release "$CI_COMMIT_SHA" --dir src
     ```
4. Hit `GET /boom`, open the error in Temps error tracking, and expand a frame —
   the surrounding Rust source is shown.

[ADR-033]: ../../../docs/adr/033-release-identity-for-symbolication.md
