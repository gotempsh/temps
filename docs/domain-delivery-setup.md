# Project domain delivery

DNS connections and delivery profiles belong to the installation. Hostnames and
their delivery configuration belong to projects and environments. A DNS zone can
therefore contain hostnames for multiple projects using different delivery
profiles.

## Configure a project

The delivery controls use separate provider choices during project creation
and in the installation-wide future-project default:

![Cloudflare and Bunny default choices](images/domain-delivery/provider-choices.png)

![Delivery choice during project creation](images/domain-delivery/project-creation.png)

The Bunny profile form shows the Pull Zone requirements before saving:

![Bunny profile setup](images/domain-delivery/bunny-profile-setup.png)

A delivery profile is a reusable provider choice, not a DNS connection. Direct
and Cloudflare profiles contain only a name and delivery mode; the DNS
connection and zone are selected for each domain during setup. Cloudflare API
credentials belong to the DNS provider connection. Bunny profiles additionally
contain a Pull Zone ID, its hostname, and an encrypted API key. Select a linked
profile name to inspect its saved configuration. To change a Bunny
Pull Zone or key, create a new profile and select it for future domain setups.

![Cloudflare delivery profile details](images/domain-delivery/profile-details.png)

For a Cloudflare DNS connection, **Add zone** offers the zones returned by that
account. Zones already managed by the connection are omitted. Verification
checks that the token can access the zone; a failed check shows an error and
does not mark the zone verified.

![Choose an available Cloudflare zone](images/domain-delivery/managed-zone-picker.png)

**Flat hostnames** keeps generated public addresses one label below the zone,
which lets Cloudflare Universal SSL cover them. Switching modes shows an impact
preview because generated routes may change. **Sync DNS records** lets Temps
create and update records for generated project addresses in that zone; custom
domain bindings are configured separately.

1. Connect the authoritative DNS provider in **DNS Providers** and add its zone.
2. Create a named **Delivery Profile**. Direct, Cloudflare proxy, and bunny.net
   CDN are supported. A Bunny profile needs an API key and Pull Zone ID; the key
   is encrypted at rest and never returned by the API. A profile does not change
   DNS records on creation.
3. When creating a project, choose **No CDN**, **Cloudflare**, or **bunny.net**.
   On an existing project's **Domains** page, choose the project default and
   environment overrides.
   These apply to subsequent setup previews; existing bindings retain their
   applied configuration. Choosing no CDN clears CDN environment
   overrides but does not remove existing DNS records.
4. Choose **Configure delivery**. Enter the hostname, environment, origin target,
   DNS provider and zone. Use the inherited profile or explicitly override it.
5. Preview the proposed DNS record and project route. Existing unmanaged records
   require explicit adoption of that individual name and type.
6. Confirm the plan. The backend rechecks ownership and preview validity before
   writing. Review the binding's status and any reported error after applying.
7. For Bunny, the setup registers the hostname on the Pull Zone, writes a CNAME
   to its system `*.b-cdn.net` hostname, and requests a free edge certificate.
   If DNS has not propagated when certificate validation runs, retry the failed
   preview after propagation. The hostname is registered only once on retries.
8. Check the public HTTPS URL and origin certificate before sending production
   traffic. `dns_configured` confirms provider DNS readback; it is not a claim
   that the application or public TLS is healthy.

Changing DNS delivery must not silently change a zone's proxy defaults, its
Cloudflare SSL mode, existing origin certificates, or certificate renewal.
Temps serves a self-signed origin certificate for generated proxied hostnames
unless an exact or matching wildcard certificate is already installed.
Cloudflare Full can use that fallback but does not authenticate the origin;
Full (strict) requires a separately installed valid exact or wildcard
certificate. Choosing a Cloudflare profile or proxy default does not change
Cloudflare's SSL mode. Provider-side DNS verification is separate from an
end-to-end HTTPS request to the application.

## Remove or change a binding

Choose **Change setup** to preview a replacement configuration. Changing a
binding’s DNS provider, zone, or record type requires removing its managed DNS
first, so the previous record is not left behind. To remove it,
choose **Remove managed DNS** and confirm the traffic impact. Cleanup checks the
record ownership, removes the managed DNS record, and preserves the custom-domain
route and certificate. A provider failure retains the binding with a
`cleanup_failed` status and a contextual error. Remove the managed DNS binding
before deleting its domain route, environment, or project. Database constraints
preserve bindings until that cleanup succeeds. A Bunny hostname that was
registered with a Pull Zone remains there after Temps removes the DNS binding;
remove it from Bunny's Hostnames panel after confirming no other route uses it.

## Provider boundaries

The DNS provider holds credentials and performs record operations. Project
readers may list delivery profile names and kinds, but DNS credentials remain
behind DNS provider permissions. The delivery adapter describes the
requirements for reaching the origin. The setup service
coordinates planning, ownership, application and status. The screens consume
the shared API and generated SDK.

Cloudflare delivery requires a Cloudflare DNS connection for the zone. Bunny
delivery works with any supported DNS provider managing a verified zone. The
Pull Zone must be enabled, have **Add Host Header** on, and use the requested
Temps edge target as its HTTP or HTTPS origin host. Bunny CNAME delivery requires
a subdomain; zone apex hostnames are rejected. The setup validates the Pull Zone
before a preview and again before an apply. Tunnel is not implemented.
Additional adapters should
provide their own delivery requirements to the common setup service, rather than
duplicate adoption rules, progress tracking, audit events, or the project UI.

The Bunny steps use the official [Pull Zone lookup](https://bunny.net/docs/api-reference/core/pull-zone/get-pull-zone),
[custom hostname](https://bunny.net/docs/api-reference/core/pull-zone/add-custom-hostname),
and [free certificate](https://bunny.net/docs/api-reference/core/pull-zone/load-free-certificate)
APIs.

The provider choice uses locally bundled brand assets. The Cloudflare logo is
`CF-Logo 1.png` from the [Cloudflare press kit](https://www.cloudflare.com/press/press-kit/)
(retrieved 2026-09-29, SHA-256 `400b70b8bb7db80da137ef8e8fcb835a692410a3083598964da08ed1756df145`).
The Bunny symbol is the [Bunny orange SVG](https://bunny-pictures.b-cdn.net/svg/Bunny-Logo-Orange.svg)
(retrieved 2026-09-29, SHA-256 `b2294f2880e525554bcd5498883cf1b0c7d02b3a3fa4f8576ae11bf538448694`).
Both are shown with provider names; the logos indicate identity, not connection
health. Bunny's setup form validates the Pull Zone through the API before saving.

## Default for future projects

On **Delivery Profiles**, choose **Cloudflare** or **bunny.net** as the new-project
default after configuring the corresponding profile. Only one can be enabled.
This setting is read when each project is created. A project creation choice
overrides it. `GET /projects/cloudflare-capability` tells project creators
whether each provider is ready and links to the missing setup.
Changing the setting does not update any existing project, domain binding, or
DNS record.

## Existing installations

The schema migration creates configuration tables. It does not adopt records,
rewrite project URLs, change public DNS, or enable CDN delivery. Existing routes
remain usable through the manual domain flow until an operator explicitly
configures delivery for them.

## Local development

Use a fresh database and data directory. Before starting an additional instance
on a shared Docker daemon, give it a unique persisted
`preview_gateway.container_name` and `preview_gateway.host_port`. Never share
encryption keys or copy a production database into this test setup.

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
Exercise Direct, Cloudflare, and Bunny separately, verify HTTPS reaches the intended
project/environment, then remove the binding and verify DNS cleanup. For a private
machine, the local automated suite works without a public IP; Cloudflare Tunnel
transport is a future adapter and is not included here.
