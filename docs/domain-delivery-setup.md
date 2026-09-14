# Project domain delivery

DNS connections and delivery profiles belong to the installation. Hostnames and
their delivery configuration belong to projects and environments. A DNS zone can
therefore contain hostnames for multiple projects using different delivery
profiles.

## Configure a project

1. Connect the authoritative DNS provider in **DNS Providers** and add its zone.
2. Create a named **Delivery Profile**. Direct and Cloudflare proxy are supported.
   A profile does not contain DNS credentials or change any records on creation.
3. Open a project's **Domains** page. Set a project default and optional
   environment overrides. These apply to subsequent setup previews; existing
   bindings retain their applied configuration.
4. Choose **Configure delivery**. Enter the hostname, environment, origin target,
   DNS provider and zone. Use the inherited profile or explicitly override it.
5. Preview the proposed DNS record and project route. Existing unmanaged records
   require explicit adoption of that individual name and type.
6. Confirm the plan. The backend rechecks ownership and preview validity before
   writing. Review the binding's status and any reported error after applying.
7. Check the public HTTPS URL and origin certificate before sending production
   traffic. `dns_configured` confirms provider DNS readback; it is not a claim
   that the application or public TLS is healthy.

Changing DNS delivery must not silently change a zone's proxy defaults, its
Cloudflare SSL mode, existing origin certificates, or certificate renewal.
Cloudflare Full (strict) requires a valid origin certificate. Provider-side DNS
verification is separate from an end-to-end HTTPS request to the application.

## Remove or change a binding

Choose **Change setup** to preview a replacement configuration. Changing a
binding’s DNS provider, zone, or record type requires removing its managed DNS
first, so the previous record is not left behind. To remove it,
choose **Remove managed DNS** and confirm the traffic impact. Cleanup checks the
record ownership, removes the managed DNS record, and preserves the custom-domain
route and certificate. A provider failure retains the binding with a
`cleanup_failed` status and a contextual error. Remove the managed DNS binding
before deleting its domain route, environment, or project. Database constraints
preserve bindings until that cleanup succeeds.

## Provider boundaries

The DNS provider holds credentials and performs record operations. The delivery
adapter describes the requirements for reaching the origin. The setup service
coordinates planning, ownership, application and status. The screens consume
the shared API and generated SDK.

Cloudflare delivery currently requires a Cloudflare DNS connection for the zone.
Bunny and Tunnel are not implemented by this change. Additional adapters should
provide their own delivery requirements to the common setup service, rather than
duplicate adoption rules, progress tracking, audit events, or the project UI.

## Existing installations

The schema migration creates configuration tables. It does not adopt records,
rewrite project URLs, change public DNS, or enable CDN delivery. Existing routes
remain usable through the manual domain flow until an operator explicitly
configures delivery for them.

## Local development

Use a fresh database and data directory. Before starting an additional instance
on a shared Docker daemon, set its persisted `preview_gateway.enabled` to `false`.
The legacy default is enabled for compatibility. Never share encryption keys or
copy a production database into this test setup.

The SDK generator accepts `TEMPS_OPENAPI_URL` so it can target the isolated
backend instead of the default development port. Supply `TEMPS_API_KEY` with a
local API key when reading the authenticated OpenAPI endpoint, or point
`TEMPS_OPENAPI_URL` at a schema file exported from that authenticated endpoint.
Regenerate from the running branch whenever handlers or schemas change:

```sh
TEMPS_OPENAPI_URL=http://127.0.0.1:19081/api/api-docs/openapi.json bun run openapi-ts
```

Keep generated credentials, database data, browser sessions and server logs
outside tracked source files. Live DNS acceptance needs an explicitly designated
test zone; local fake-provider tests must not access real provider credentials.

## Run the automated acceptance tests

From the Rust workspace root:

```sh
cargo check --lib -p temps-dns -p temps-core -p temps-agents
cargo test --lib -p temps-dns
cargo test -p temps-dns --test domain_delivery_integration -- --nocapture
```

The integration suite creates isolated PostgreSQL databases using testcontainers.
Docker must be available to execute the assertions; unavailable Docker is reported
as an explicit skip. The suite covers migrations and constraints, project and
environment defaults, scope rejection, and complete preview/apply with a fake DNS
provider, including provider failure and retry. No real DNS credentials are used.

For a public acceptance test, use a dedicated test zone and a reachable origin.
Exercise Direct and Cloudflare separately, verify HTTPS reaches the intended
project/environment, then remove the binding and verify DNS cleanup. For a private
machine, the local automated suite works without a public IP; Cloudflare Tunnel
transport is a future adapter and is not included here.
