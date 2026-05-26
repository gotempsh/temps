## Pass summary — run 20260526-200000

This pass filled the Teams & Collaboration stub with accurate, sourced content drawn from the permissions.rs Role enum (six roles: Admin, User, Reader, ApiReader, Mcp, Custom), the audit_logs entity, and the OIDC handler — covering roles, permissions matrix, user creation from dashboard/CLI/API, API key scoping, audit log, and SSO/OIDC. It also rewrote the Security Features page, which had incorrect role names (Owner/Admin/Member/Viewer instead of Admin/User/Reader), wrong permission strings (read:projects instead of projects:read), missing lead paragraph and anchor IDs, and informal prose ("That's It").

### Risk
REVIEW

### Files changed
- `docs/features/teams/page.mdx` — Stub filled: complete Teams & Collaboration guide sourced from codebase permissions.rs, auth schema, and audit_logs entity
- `docs/architecture/security/page.mdx` — Clarity rewrite: corrected role names and permission string format, added lead paragraph and section anchors, rewrote informal prose, replaced deprecated whitelist/blacklist terminology, added admin listener isolation section

### Stub filled
features/teams — full Teams & Collaboration guide covering the six built-in roles with sourced permissions, user creation (dashboard/CLI/API), scoped API keys with concrete permission sets, audit log, and OIDC/SSO

### Clarity rewrite
architecture/security — fixed incorrect role names and permission format, added frontmatter sections array, lead paragraph, and anchor IDs; replaced informal headings and deprecated allow/deny list terminology; corrected security headers table

### Stale refs fixed
none this pass
