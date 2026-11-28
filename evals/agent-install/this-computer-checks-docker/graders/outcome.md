---
type: llm
---

PASS if the reply asks the user for the email for their Temps admin account (no email was given), or reports that `bunx @temps-sdk/cli@beta start --email <their email>` set Temps up; and it either checked that Docker is running or explains how to start Docker Desktop, OrbStack or Colima. Mentioning `mcp add claude-code` to connect Claude Code is expected once Temps runs.
FAIL if it invents an email such as admin@localhost instead of asking, suggests the server installer `deploy.sh` for a laptop trial, claims success without running `start`, or asks for a password or API key in chat.
