# CLAUDE.md

All rules for this repository live in [`AGENTS.md`](./AGENTS.md), shared by every
coding agent, and are imported here so Claude Code loads them at startup:

@AGENTS.md

## Claude Code specifics

- Repository skills live in `.agents/skills/` (each a `SKILL.md`), not
  `.claude/skills/`. When a task matches one (add a migration, handler or plugin,
  split a module, console UI, start the server), read its `SKILL.md` first and
  follow it.
- `web/CLAUDE.md` imports [`web/AGENTS.md`](./web/AGENTS.md), so the console
  rules load when you work under `web/`.
- Keep this file to Claude-specific notes. A rule for everyone goes in
  `AGENTS.md`.
