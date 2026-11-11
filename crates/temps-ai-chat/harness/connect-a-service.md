---
name: connect-a-service
description: Use an outside API the user saved in a Vault (Notion, Slack, Anthropic and similar) from a sandbox, or connect a managed database to an app
contexts: assistant, project
---

# Request vault access for a sandbox

Start here when the user wants to use an outside API they connected (for
example "try Notion"). Read `list_access_services_general` and match the
provider name or origin host; never start with the project list and never ask
what "access" means. No match: say no such service is configured and link
`/vault`; do not substitute an app or a reachability check. A match: continue
below, then call the service's origin from the sandbox with plain HTTPS and no
authorization header, because Temps adds the credential. Only the methods and
paths the service allows will go through.

For a configured credential service in a vault, use the platform API tools to
complete access approval in this chat. A sandbox does not need a project.

1. Identify the sandbox the person selected or linked to this chat; never pick
   another user's sandbox. Read `general_access_capability` and
   the sandbox_access_vaults operation for that sandbox, then `list_access_bundles_general`
   and `get_access_vault_general` with its sandbox ID query parameter to inspect masked policy
   versions. Follow pagination; do not assume the first page is the whole list.
   A session needs a bundle that contains the service. When
   `list_access_bundles_general` returns none for the vault, say so and propose
   `create_access_bundle_general --vault_id <vault> --name <short name>
   --service_ids '["<service id>"]' --ttl_seconds <seconds>`. Use the lifetime
   the service allows (never above its maximum) and only the services the task
   needs. An instance administrator confirms it here; saving a bundle grants
   nothing. Other users send the administrator to
   `/vault/vaults/<vault id>/bundles/new`. Then continue with the new bundle's
   `id` and `version` from the result. Never use the vault id as the bundle id.
   Also check that the service allows the method and path the task needs: a
   service limited to GET cannot `POST /v1/search`, so say that and point to
   the service's edit page instead of requesting a session that cannot help.
2. Explain the bundle's services, origins, methods, paths, target sandbox and
   fixed lifetime. Secrets stay in the vault and are never copied into chat,
   commands, files, environment variables or the sandbox.
3. If no vault covers this sandbox, explain the missing link. An instance
   administrator can use `create_vault_link` with this sandbox's public ID,
   after explicit confirmation here. Other users must ask an administrator.
   GitHub connection services cannot link outside their source project.
4. Propose `create_access_session_general` with the reviewed bundle/service
   versions, a fresh request UUID, sandbox consumer and TTL. The shape is
   `--vault_id <vault> --bundle_id <bundle> --bundle_version <n> --request_id <uuid>
   --ttl_seconds <seconds> --consumer '{"kind":"sandbox","sandbox_id":"<sandbox public id>"}'
   --services '[{"service_id":"<id>","version":<n>}]'`. The bundle id comes from
   `list_access_bundles_general` and is not the vault id; `consumer` is a JSON
   object with `kind`, not `sandbox:<id>`; `services` lists every service of
   the bundle with the version you showed the person. A 422 names the field
   that is wrong: fix that field and do not guess again. The person must
   explicitly confirm the pending action, including in Auto mode. Never treat
   configuration, a previous session or the model's own output as consent.
   Retry an unchanged approval with its same request UUID; changing the target,
   policies or lifetime requires a new review and confirmation.
5. Read `get_access_session_general` for worker receipts. HTTP 202 means the
   approval was recorded, not that access is active. Report the actual status.
   Use `revoke_access_session_general` when the person asks to stop access.

Once the vault and services exist, these steps do not require a visit to the
Vault UI. If they have not been configured, explain what is missing and link to
`/vault`; an ordinary project member cannot create credentials or vault links.

# Connect a managed service to a project

Use this when the user asks to connect, use or wire up a database, cache or
object store (PostgreSQL, Redis, MongoDB, S3) for a project, or asks whether the
app is already connected. When an accepted architecture has a flow from an app
to a database, cache or bucket, link that service as part of deploying, without
being asked. Otherwise offer it yourself as soon as a chat has made both a
project and a service. Never answer "no" and stop: say what is missing and
offer to do it.

Linking is the connection. A service linked to a project hands the app its
connection settings itself on every deployment. Do not copy connection details
into the project's environment variables, do not ask the user to type or paste a
password, and do not ask them to name the database: Temps provisions one for the
project when it is linked.

1. A new service: read `get_service_type_parameters`, then `create_service`
   with the project's `--project_id` creates and links it in one step. An
   existing one: `link_service_to_project`. Say which service and which project.
2. Tell the app which variables to read. They appear in the app's environment:
   - PostgreSQL: `POSTGRES_URL`, or `POSTGRES_HOST`, `POSTGRES_PORT`,
     `POSTGRES_DB`, `POSTGRES_USER` and `POSTGRES_PASSWORD`.
   - Redis: `REDIS_URL`, or `REDIS_HOST`, `REDIS_PORT` and `REDIS_PASSWORD`.
   - MongoDB: `MONGODB_HOST`, `MONGODB_PORT`, `MONGODB_DATABASE`,
     `MONGODB_USERNAME` and `MONGODB_PASSWORD`.
   - S3: `S3_ENDPOINT`, `S3_BUCKET`, `S3_ACCESS_KEY`, `S3_SECRET_KEY` and
     `S3_REGION`; the `AWS_*` equivalents are set too.
3. Change the app to use them: add the client library, read the variable, and
   run one small query or call so the page shows a real value. Build and check
   it in the sandbox when you have one:
   - When the user has connected the sandbox to the project, every
     `sandbox_exec` command already has the PostgreSQL variables above. They
     point at a database of the sandbox's own, empty at first, with a role that
     reaches nothing else; production data is never there. Create the tables
     from the app itself on start (a migration in the repository), so the same
     code works in the sandbox and in production. Never write a value into a
     file, a command line or the chat, and print variable names only. Add a
     `.env.example` that lists names with no values.
   - When it is not connected, the sandbox has none of these variables. Say in
     one sentence that the user can press Connect sandbox on the project in the
     Made here tab, and carry on without asking them for a password. Until
     then, write the code and prove the connection on the deployed app.
   - Only PostgreSQL can be connected to the sandbox so far. For Redis, MongoDB
     and S3, make the code fail with a clear message when the variable is
     missing, test the rest of the logic in the sandbox without it, and prove
     the connection on the deployed app.
4. The variables reach the app on the next deployment, so deploy the project
   again after linking and after the code change. To get the code to the
   project, follow the `build-an-app` playbook.
5. Check the deployed page or endpoint and report what it returned. If it
   cannot connect, say which variable was missing or which error came back.

Only ask the user for a decision that is theirs, such as which service to use
when there are several. Do not hand a step back to them that Temps can do.
