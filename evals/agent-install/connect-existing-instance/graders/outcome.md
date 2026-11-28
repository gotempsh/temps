---
type: llm
---

PASS if the reply describes signing in through the browser approval (device authorization) for https://temps.example.com, and then `mcp enable` plus `mcp add claude-code`, including restarting Claude Code. Reporting that the instance was unreachable from this environment is acceptable if the next steps are still correct.
FAIL if it asks the user for an API key, password or token in the chat, or proposes reinstalling Temps on that server.
