# Go Error Tracking Example (with source context)

A minimal Go app that reports errors to Temps error tracking and — once you
upload its source — shows the actual source code around each stack frame.

## What it demonstrates

- Wiring `sentry-go` against the Sentry-compatible DSN Temps injects as
  `SENTRY_DSN`.
- The **correct release setup** for a compiled binary (see [ADR-033]): `Release`
  is left unset so the SDK reads `SENTRY_RELEASE` from the environment, which
  Temps injects with the deployed commit SHA. Hard-coding it would break the
  source-context join.
- `AttachStacktrace: true`, so plain `fmt.Errorf` errors still carry a stack
  trace.

## Endpoints

- `GET /` — hello
- `GET /health` — health check
- `GET /boom` — deliberately reports an error to Temps

## Running locally

```bash
go mod download
SENTRY_DSN="<your-temps-dsn>" go run main.go
# then: curl localhost:8080/boom
```

## Seeing source code in the stack trace

1. Deploy this app to Temps (git-based deploy).
2. In the project's **Settings → General → Error Tracking Source Context**,
   turn the toggle **on**.
3. Get the source to Temps, keyed by the release the app reports
   (`SENTRY_RELEASE` = the deployed commit SHA):
   - **Automatic** (git deploys): with the toggle on, Temps captures the source
     from the build automatically — nothing to do.
   - **Manual / CI:** upload it yourself, keyed by the same release:
     ```bash
     bunx @temps-sdk/cli errors source-files upload \
       --project-id <id> --release "$CI_COMMIT_SHA" --dir .
     ```
4. Hit `GET /boom`, open the error in Temps error tracking, and expand a frame —
   the surrounding Go source is shown.

[ADR-033]: ../../../docs/adr/033-release-identity-for-symbolication.md
