# Observability onboarding

Use this workflow after the capability detector reports one or more missing
pillars, or when the user explicitly asks to add Temps observability.

## 1. Establish scope

Summarize what is already configured and offer only what is missing:

- Product analytics: page views, product events, Web Vitals, optional replay.
- Error tracking: uncaught and handled exceptions with releases/source maps.
- OpenTelemetry: server traces, metrics, and structured correlated logs.

Ask whether to add all recommended capabilities, selected capabilities, or
continue without changes. Explain that instrumentation changes dependencies and
source files but Temps supplies destinations to applications deployed through
Temps.

## 2. Apply shared hygiene

Read [../references/observability/hygiene.md](../references/observability/hygiene.md).
Keep server credentials out of browser bundles, redact sensitive attributes,
bound attribute cardinality, exclude health traffic, batch exports, and flush
telemetry during graceful shutdown.

## 3. Load one framework guide

| Evidence | Guide |
|---|---|
| Next.js, React, Vite, Remix, Node | [javascript-typescript.md](javascript-typescript.md) |
| `pyproject.toml`, `requirements.txt`, Django, Flask, FastAPI | [python.md](python.md) |
| `Cargo.toml` | [rust.md](rust.md) |
| Go, Vue, Svelte, Angular, Ruby, Java, PHP, .NET, Flutter | [framework-routing.md](framework-routing.md) |

Do not combine snippets from multiple framework guides unless the repository is
actually a polyglot service.

## 4. Verify real delivery

1. Run the project's existing build/typecheck/tests.
2. Start or deploy the application using its normal workflow.
3. Send a synthetic analytics event named `temps_skill_verification` when
   analytics was added.
4. Capture a handled exception whose message begins
   `temps-skill-verification` when error tracking was added.
5. Exercise a normal request and locate its trace when tracing was added.
6. Query the Temps UI or read-only CLI command for the target project.
7. Remove temporary verification triggers from application code while keeping
   the resulting test evidence in the report.

If delivery fails, diagnose SDK initialization, endpoint/protocol, target
project, credential type, rate limits, quota, and proxy routing in that order.
