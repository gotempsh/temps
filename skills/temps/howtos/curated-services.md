# Provision a curated service

Use this journey when the user wants a catalog application, such as Keycloak or
Browserless, rather than deployment of their own source. The console entry is
**Projects → New service → Services**. These are curated app templates, distinct
from the managed databases, caches, and object stores in `services` /
`external-services`.

## Discover before building

1. Select the intended Temps instance and inspect existing services and managed
   databases to avoid duplicates. With connected tools, use an instance context
   for a new service; an unrelated workspace project link may inject its own
   environment filters into later queries.
2. Inspect the live catalog and matching template: image, entrypoint, health
   path, resources, variables, secret generators, and database bindings. Prefer
   available connected Temps tools. The API exposes `GET /templates` and
   `GET /templates/{slug}`; the pinned CLI exposes `templates list --kind app`.
   Read [the CLI safety contract](../references/cli-runtime.md) and
   [template commands](../references/commands/templates.md) when using the CLI;
   runtime help is authoritative.
3. Prefer a matching curated template that meets the request. Use its pinned
   image and native database bindings rather than writing a Dockerfile or
   manually copying connection credentials. Check documented runtime overrides
   if a specific requirement differs. Use custom source/image deployment when
   the catalog lacks a suitable template or the user explicitly needs it; state
   the reason. Do not invent a template or silently substitute another app.

## Create through Temps

- Use the native template-creation operation, not a generic empty project plus
  a custom build. Connected API tools expose
  `POST /templates/{slug}/instantiate`; the CLI exposes `templates create <slug>`.
  Confirm the selected tool's current request schema before writing.
- A single-service template can be created standalone. Multi-service templates
  can create or join a project group; see
  [projects and monorepos](projects-and-monorepos.md). Read actual database
  requirements from the template: reuse an appropriate existing managed service
  with its service ID or let the template create one. Do not pre-create a
  duplicate database just because the application needs one.
- Explain the instance, service names, database choice and runtime overrides
  before creation. Use template-generated secrets and platform-managed bindings
  where available; follow the secret-handling rules for supplied values.
- Creation runs server-side and queues the initial deployments. It does not
  require a checkout, Git connection, local Docker build, or workspace project
  link. A workspace upload/deploy tool's linking requirement applies to that
  source workflow; it is not a prerequisite for the native service operation.
- Reuse an idempotency key for retries. If waiting is interrupted, inspect the
  returned instantiation ID and resume/retry that run rather than creating
  another service. See [instantiations](../references/commands/instantiations.md).
  If the available tool/runtime lacks this operation, report the capability gap
  instead of silently replacing the curated path with custom infrastructure.

## Verify creation and deployment separately

1. Follow the instantiation through completion. That confirms creation and
   deployment dispatch, not that the application is ready.
2. Inspect each returned service/project ID and its own environment ID. Confirm
   template identity, database linkage and required configuration without
   revealing credentials. Do not query the new service with another project's
   default environment filter.
3. Follow the actual deployment to completion, inspect bounded logs on failure,
   and request the configured health path at the returned service URL. Report
   pending or failed deployment honestly even if instantiation succeeded.
4. Distinguish a running service from completed integration. For an identity
   provider, verify HTTPS and the issuer/callback configuration before connecting
   SSO or enrolling passkeys. Follow the relevant domain or backup journey for
   remaining setup; do not claim TLS or backups merely because the template or
   database exists. Report those prerequisites separately from provisioning.
