# Deploy an application

Use this journey for a repository that should become a Temps project or a new
deployment of an existing project.

1. Inspect the framework, package manager, lockfile, start/build commands,
   health route, required environment-variable names, and existing
   `.temps.yaml`. Read [the runtime contract](../references/runtime-contract.md).
2. Run the router's capability checkpoint. Offer missing observability once;
   continue when the user declines.
3. Inspect the named target context and existing projects with the pinned CLI.
   A target context identifies a Temps server; it does not prove which project
   environment is production. Never guess the server, project, environment,
   branch, or commit.
4. If no project exists, read
   [the projects reference](../references/commands/projects.md). For deployment
   syntax, read [the deploy reference](../references/commands/deploy.md).
5. Explain the exact repository, project, environment, context, and health
   check before creating or deploying.
6. Preserve the application's package manager and lockfile. Put secrets in the
   Temps environment manager, never in `.temps.yaml` or source.
7. Verify with the deployment list/status, bounded logs, and the deployment URL
   returned by structured output. Request the configured health endpoint on
   that host. A successful CLI exit alone is insufficient.

For migration from another host, additionally read
[the migrate reference](../references/commands/migrate.md). Inventory domains,
environment variable names, databases, object stores, cron/background workers,
and health checks before changing traffic.
