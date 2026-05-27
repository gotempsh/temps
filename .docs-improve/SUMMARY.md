## Pass summary — run-2026-05-27-001

Filled the `advanced/networking` stub with a complete guide covering port configuration (bind address requirements, per-environment port overrides), internal container networking, managed service networking, multi-node networking (agent port 3100, private_address), proxy configuration (single-listener vs split-listener mode, nginx reverse-proxy passthrough headers), IP access control and Attack Mode, WireGuard private tunnels for cross-datacenter multi-node clusters, replica-level load balancing (round-robin via Pingora, no sticky sessions, health checks), and host firewall rules with a ufw example. Also added a `sections` export and proper lead paragraph plus section anchors to `architecture/data-flow` (clarity pass), and removed marketing "Competitive Advantage" language from Note blocks in `features/cron-jobs` and `features/managed-services` (grammar/phrasing fix).

### Risk
REVIEW

### Files changed
- `docs/advanced/networking/page.mdx` — stub filled: complete networking guide (ports, internal networking, proxy config, IP access control, WireGuard, load balancing, firewall rules)
- `docs/architecture/data-flow/page.mdx` — clarity pass: added `sections` export, lead paragraph with `{{ className: 'lead' }}`, `---` dividers, and lowercase anchor headings to match site conventions
- `docs/features/cron-jobs/page.mdx` — grammar fix: removed "Competitive Advantage" marketing label from Note block
- `docs/features/managed-services/page.mdx` — grammar fix: removed "Competitive Advantage" marketing label from Note block

### Stub filled
advanced/networking — complete guide to port config, container networking, split-listener proxy, IP ACLs, WireGuard tunnels, load balancing, and host firewall rules

### Clarity rewrite
architecture/data-flow — added sections export, lead paragraph, horizontal rules, and normalised heading anchors; the page was missing all structural scaffolding present on every other architecture page

### Stale refs fixed
none this pass
