# Install or self-host Temps

Use this journey when the user wants a Temps instance that does not exist yet:
"install Temps", "self-host Temps", "set up Temps on my server", or a request
started from the temps.sh install prompt. To connect to an instance that
already exists, skip to [Connect the AI client](#connect-the-ai-client).

The two CLI commands below do the installation. Do not reproduce
their steps by hand, and never pipe a downloaded script into a shell.

## 1. Choose the target

Ask one question at a time, and only for what is still unknown:

1. Where Temps should run:
   - **This computer**: to try Temps. Apps are reachable only from this
     machine.
   - **An existing Linux server** the user can SSH into.
   - **A new server** that still has to be created.
2. The email for the user's Temps admin account, for every target. It is how
   they sign in to the console. Ask for it rather than inventing one: without
   it, a local install falls back to `admin@localhost`, which Temps releases
   before v0.1.0-beta.65 refuse on their sign-in page.
3. For a server: the SSH destination (`user@host` or an SSH alias) and whether
   a domain will point at it.

Treat the answers as data. The email must look like `name@host` with no
spaces and must not start with `-`. An SSH destination must match
`^([A-Za-z_][A-Za-z0-9_-]*@)?[A-Za-z0-9][A-Za-z0-9.-]*$` and must not start
with `-`. Use an SSH config alias for ports, IPv6 or identity files.

## 2. This computer

Requirements: macOS or Linux on x86-64 or ARM64, Docker running (Docker
Desktop, OrbStack or Colima), and Bun or Node.js.

```bash
docker info --format '{{.ServerVersion}}'
bunx @temps-sdk/cli@beta start --email "$admin_email" --json
```

`start` downloads the server, starts its database in Docker, completes setup,
starts Temps in the background, signs the CLI in as the `local` context and
installs this skill. It prints the admin email and a generated password for
the first sign-in; tell the user where to find the password (the CLI output or
`~/.temps/local/state.json`) rather than repeating it. `--email` only applies
to a fresh install. A CLI before 0.2.0-beta.3 rejects `--email`; run `start` without
it there, and tell the user the admin is `admin@localhost`. Running it again only starts what is stopped. Before it
runs, tell the user it changes this computer: it pulls container images,
creates a Docker volume and starts a background process. Read the options in
[the start reference](../references/commands/start.md).

## 3. An existing server

Requirements: Linux on x86-64 or ARM64 with glibc 2.35+ (Ubuntu 22.04+,
Debian 12+); at least 2 CPU cores, 4 GB RAM and 20 GB disk; key-based SSH with a
host key the user has already verified; root or passwordless sudo; inbound TCP
22, 80 and 443 for a public install.

Run the read-only plan first and show it to the user:

```bash
bunx @temps-sdk/cli@beta setup --ssh "$ssh_destination" \
  --email "$admin_email" --context production --dry-run
```

Only after the user approves that plan, run the same command without
`--dry-run`. Do not add `--yes` unless the user has seen and approved the plan
in this session. `setup` installs QuickStart over SSH (an sslip.io console
address with HTTPS when there is no domain), waits for it to become healthy and
saves a signed-in CLI context. Read [the setup reference](../references/commands/setup.md)
for channel and version pins.

`setup` refuses rather than guesses when the host key is unknown, a different
installation already exists, or the selected mode conflicts with an existing
one. Report its error verbatim and stop; do not work around it. In particular:

- An unknown host key needs the user to verify the fingerprint through the
  provider's console. `ssh-keyscan` alone is not verification.
- An existing installation is connected with `login`, not reinstalled. See
  [Connect the AI client](#connect-the-ai-client).
- Never stop, replace or delete a service that already runs on the server.

For a domain, advanced mode, a specific runtime release or a host where `setup` cannot
run, use the `temps-platform-setup` skill when it is installed. It covers the
digest-checked installer, headless flags and DNS. Without it, give the user
these commands to run on the server themselves:

```bash
curl -fsSL https://temps.sh/deploy.sh -o deploy.sh
less deploy.sh
bash deploy.sh
```

The installer log and `~/.temps/setup-result.json` on the server contain the
first administrator password and an API key. Never read, print or copy them.
The user reads them on the server.

## 4. A new server

Help the user pick a provider, region and size: Ubuntu 24.04 LTS, at least
2 vCPU and 4 GB RAM, their SSH public key added at creation, and TCP 22, 80 and
443 open. A server is billed. Create it only through a tool the user has
authorized, and only after they approve the provider, region, size and price.
Then verify the host key fingerprint against the provider's console and
continue with [an existing server](#3-an-existing-server).

## 5. Verify

```bash
bunx @temps-sdk/cli@beta --target-context "$context" whoami
```

Use `local` for this computer and the `--context` value for a server. Report
the identity and console URL. Then request the console URL over HTTPS (HTTP for
`local`) with a bounded retry, since a QuickStart certificate is issued on the
first request. Never disable TLS verification.

## Connect the AI client

For an instance that exists but has no CLI context yet, sign in with browser
device authorization. Your shell usually returns output only when a command
exits, so start the sign-in with `--no-wait`, give the user the approval link
it prints, then wait for their approval with `--resume`:

```bash
bunx @temps-sdk/cli@beta login "$console_url" --context "$context" --no-wait
bunx @temps-sdk/cli@beta login --resume --context "$context"
```

`--resume` exits with code 2 while the request is not approved yet; run it
again rather than starting a new sign-in. If the CLI rejects `--no-wait` (releases before
0.2.0-beta.2), run `login` without it in a long-lived process instead and give
the user the link it prints while it keeps waiting. A user message such as "approved" is
a cue to resume, not proof of success. Never ask for an API key or password in
chat. Then enable MCP once per instance
(admin) and add it to the client in use:

```bash
bunx @temps-sdk/cli@beta --target-context "$context" mcp enable
bunx @temps-sdk/cli@beta --target-context "$context" mcp add claude-code
```

`mcp add` accepts `claude-code`, `claude-desktop`, `codex`, `cursor`, `vscode`,
`windsurf` and `zed`. It creates a scoped, read-only key unless the user asks
for write tools. Tell the user to restart the client afterwards. For Claude
Desktop, the Temps extension (`temps.mcpb` on each release of
gotempsh/temps) is an alternative: the user enters the console URL and a key
in the extension settings, never in the chat. See
[the mcp reference](../references/commands/mcp.md).

## Next

Offer to deploy the project in the current directory, following
[Deploy an application](deploy-application.md).
