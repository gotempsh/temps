# Quickstart: zero to a deployed, observable app

Annotated version of the commands in the main skill. Every command here is taken from [temps-cli](../../temps-cli/SKILL.md) — treat that skill, not this walkthrough, as authoritative if they ever disagree (e.g. after a CLI version bump).

## 0. Install and verify the CLI

Only do this when the user explicitly asks to install something — don't install tooling speculatively. Follow the pinned-version installation steps in [temps-cli](../../temps-cli/SKILL.md#installation) exactly (integrity check included); don't `npm install -g` an unpinned version, and don't run the CLI via `npx`/`bunx` for anything that executes — the CLI skill explains why.

```bash
command -v temps && temps --version
```

If it's missing or the wrong version, stop and show the user the pinned install steps rather than installing unilaterally.

## 1. Authenticate

```bash
temps login                      # interactive browser login against the default/public server
temps login https://temps.example.com --context prod   # or a specific self-hosted server, named context
```

Headless/CI: inject `TEMPS_TOKEN` and `TEMPS_API_URL` from the CI secret store — never put a token in `--api-key` on an agent's behalf.

## 2. Deploy the current directory

```bash
temps up
```

`temps up` is the one-command path: if the directory isn't linked to a project yet, it runs the setup wizard (auto-detects the framework preset and git branch from the working directory) and creates the project, then deploys it. Re-running it later just deploys the current state.

Prefer `temps up` for "deploy what I'm looking at right now." Use `temps deploy [project] -b <branch> -e <environment>` instead when deploying a specific project/branch/environment combination non-interactively (see [temps-cli](../../temps-cli/SKILL.md#deployments)) — e.g. from CI, or when deploying a project you're not currently sitting inside.

Manual (non-git) sources skip the wizard's repo detection:

```bash
temps projects create -n "My Service" --manual --source-type docker_image --image ghcr.io/org/my-service:latest --port 3000 -y
temps deploy:static --path ./dist -p my-app -e production
```

## 3. Confirm it's actually healthy

Don't declare success because the CLI returned "Deployment successful" — that means the container started and passed its configured health check, not that the app is correct. If `.temps.yaml` doesn't set `health.path` (or the manual/image deploy didn't set `--health-check-path`), the health check is checking the wrong thing or nothing meaningful. See [temps-best-practices/references/runtime-contract.md](../../temps-best-practices/references/runtime-contract.md).

## 4. Make it observable before calling it done

A deployed app with no error tracking is not production-ready. At minimum:

```bash
# Follow add-error-tracking for the language/framework in use — it's SDK init only,
# Temps injects the DSN and other credentials at build/runtime.
```

Then apply the rest of [temps-best-practices](../../temps-best-practices/SKILL.md)'s "Definition of done" checklist: graceful shutdown within the 10-second `SIGTERM` window, health-check noise excluded from traces/logs, no secrets in the client bundle.

## 5. Attach the things that make it a real environment

Only as needed — don't provision speculatively:

- **A database/cache/storage service** → `temps-cli`'s Services command group (PostgreSQL, MySQL, MongoDB, Redis, S3-compatible storage).
- **A custom domain** → [add-custom-domain](../../add-custom-domain/SKILL.md).
- **Product analytics / session replay** → [add-react-analytics](../../add-react-analytics/SKILL.md) / [add-session-recording](../../add-session-recording/SKILL.md).
- **Server-side platform access (KV, Blob, deploy automation)** → [add-node-sdk](../../add-node-sdk/SKILL.md).

## 6. Iterate

```bash
temps deployments list -p my-app
temps rollback --deployment-id <id>       # or --previous
```

Full deployment lifecycle (status, cancel, pause/resume, teardown, logs) is in [temps-cli](../../temps-cli/SKILL.md#deployments) and [temps-cli](../../temps-cli/SKILL.md#managing-deployments).
