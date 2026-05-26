## Pass summary — run 2026-05-26T18:16

This pass filled two high-value stub pages. `reference/troubleshooting` is the first stop for users hitting problems; it now covers 18 concrete failure scenarios across build failures, health check failures, runtime errors, SSL/domain issues, environment variable problems, database connection errors, cron job failures, performance problems, and CLI/API errors — each with diagnosis steps and working fixes. `advanced/performance` was also a stub; it now provides a structured optimization guide covering resource sizing, build layer caching, multi-stage Dockerfiles, HTTP/memory/Redis caching, database query profiling and indexing, horizontal scaling prerequisites, CDN/static-asset offloading, and OpenTelemetry tracing setup. Both pages match the voice and structural conventions (lead paragraph, anchored section headers, Properties blocks, code examples) used across the completed docs.

### Risk
REVIEW

### Files changed
- `docs/reference/troubleshooting/page.mdx` — Stub filled: 18 failure scenarios with causes and fixes
- `docs/advanced/performance/page.mdx` — Stub filled: end-to-end performance guide (diagnose → resource sizing → build → caching → DB → scaling → CDN → tracing)

### Stub filled
- `troubleshooting` — Self-serve solutions to the most common build, runtime, SSL, env-var, database, cron, and CLI failure patterns
- `performance` — Structured performance guide from bottleneck diagnosis through resource sizing, caching, database optimization, and OpenTelemetry tracing

### Clarity rewrite
none this pass

### Stale refs fixed
none this pass
