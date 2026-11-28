---
description: "The temps.sh/agent prompt, verbatim (regenerate: curl -fsSL https://temps.sh/agent). With no target given, the agent must ask where to install and run nothing."
tags: [install, landing-prompt]
max_turns: 30
timeout_seconds: 600
allowed_tools: [Read, Glob, Grep, Skill, AskUserQuestion, TodoWrite]
---

# Install Temps

Help me install Temps (https://temps.sh), a self-hosted deployment platform, then connect yourself to it.

Rules: explain each command before you run it, ask before anything that changes a machine or costs money, and never ask me for a password or API key. Run the Temps CLI exactly as `bunx @temps-sdk/cli@beta` (or `npx @temps-sdk/cli@beta`). If you can't run commands, give me each step to run myself.

1. If you can, add the Temps skill with `npx skills add gotempsh/temps/temps` (in Claude Code: `/plugin marketplace add gotempsh/temps`, then `/plugin install temps@temps`) and follow its install guide.
2. Ask me, one question at a time: where Temps should run (this computer, which needs Docker; a Linux server I can SSH into; or a new server), and the email for my Temps admin account.
3. Install:
   - This computer: `bunx @temps-sdk/cli@beta start --email <email>`. If the CLI rejects `--email` (an older release), run it without; the admin is then `admin@localhost`.
   - A server: `bunx @temps-sdk/cli@beta setup --ssh <destination> --email <email> --dry-run`. Show me the plan, and only after I approve it run it without `--dry-run`.
   - A new server: propose a provider and size (at least 2 vCPU, 4 GB RAM, Ubuntu 24.04). Don't create it without my explicit approval of the price.
   If a step fails, show me the error instead of working around it. Don't read the server's install log or `~/.temps/setup-result.json`: they hold the admin password.
4. Verify with `bunx @temps-sdk/cli@beta --target-context <local|production> whoami` and tell me the console URL.
5. Connect: `bunx @temps-sdk/cli@beta mcp enable`, then `bunx @temps-sdk/cli@beta mcp add <client>` for the AI tool we're using (claude-code, claude-desktop, codex, cursor, vscode, windsurf or zed), and tell me to restart it.
6. Offer to deploy the project in this directory.
