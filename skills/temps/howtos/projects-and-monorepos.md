# Group services into a project, import a monorepo

Terminology first, because the CLI and the console use different words:

| Console | CLI | What it is |
|---|---|---|
| **Service** | `projects` | One deployable app: its own builds, environments, domains, logs. |
| **Project** | `project-groups` | A group of services that ship together and share a repository, variables and path routes. |
| (databases, caches) | `services` | Managed Postgres, Redis, S3 and so on. Not the same as a Service. |

Read [the project-groups reference](../references/commands/project-groups.md)
for syntax, and [the instantiations reference](../references/commands/instantiations.md)
when an import or a multi-service template is involved. Confirm the reviewed
runtime has the command group first (`project-groups --help`); if it does not,
report the version gap instead of creating services one by one.

## Choose the journey

- **Several apps in one repository (monorepo).** Create a project, set its
  repository and branch (`project-groups repo set`), preview with
  `project-groups import <project> --dry-run`, then import. Each app becomes a
  service that follows the project's repository and keeps its own directory.
- **Existing services that belong together.** Create a project and add them
  (`project-groups add`). Nothing is redeployed; shared variables apply on each
  service's next deployment.
- **An app made of several services from the template catalog.**
  `templates create <slug>` creates every service, its databases, shared
  variables and routes in one server-side run.
- **A frontend that calls a same-origin `/api`.** Add a path route
  (`project-groups routes add <project> <frontend> /api <backend>`) instead of
  CORS or a custom proxy.

## Rules

1. Inspect first: `project-groups list`, `projects list`, and for an import the
   dry run. Show the user which services will be created or moved.
2. Adding a service moves it out of any other project and removes the path
   routes it had there. Say so before running `add`.
3. Shared variables: a service's own variable with the same name wins; shared
   ones override linked-database defaults. Previews only receive shared
   variables opted in with `--preview`. Names are case-sensitive.
4. Never put secret values on the command line: omit the value to get a hidden
   prompt, or pipe it with `--value-stdin`, and mark it `--secret`.
5. Imports and template creations run on the server. If waiting stops or
   fails, continue with `instantiations show <id> --wait` or
   `instantiations retry <id>`; never re-run the import, which would create
   duplicates.
6. Verify with `project-groups show <project>`, `projects group -p <service>`
   and `project-groups vars inherited <service>`.
