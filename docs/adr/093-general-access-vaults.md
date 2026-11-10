<!--
SPDX-FileCopyrightText: 2024-2026 Temps Contributors
SPDX-License-Identifier: MIT OR Apache-2.0
-->
# ADR-093: General access vaults with explicit consumer links

**Status:** Proposed
**Date:** 2026-11-10

## Context

Temps currently makes credential services, vaults and bundles project-owned.
Session approval, grant delivery and worker policy planning require the sandbox
and credential service to share that project. Projectless sandboxes cannot use
these credentials. This decision supersedes, for Temps, the project-owned vault
rule described in the Tenancy section of Tokenless ADR 0002. Session consent,
expiry and worker enforcement remain separate from vault configuration.

## Management and use

Services, vaults and bundles are instance-level resources. Only an instance
administrator can create, edit or delete their configuration, credentials and
links. Project membership never grants management authority. Admin-only
management avoids accidentally inheriting project write permissions when a
credential becomes reusable across projects. Each management handler also checks
its normal instance permission and rejects deployment tokens; an API key cannot
exceed its own permission ceiling merely because its owner is an administrator.
Secrets remain encrypted at rest and masked in every read response and audit.

A vault has independently identified links to projects or individual sandboxes.
Exactly one consumer kind is set on each link. Unique constraints prevent two
active links to the same consumer. A link permits requesting access; it does not
issue a credential or approve a session.

Approval requires a current covering link and sandbox ownership or administrator
status. A project link additionally requires the existing ProjectAccessChecker
and applicable project permissions. A direct sandbox link does not confer project
membership. It can cover a projectless sandbox. Sandbox write permission and
human consent remain mandatory. Non-managers see only the vault metadata and
bundle policies available to their authorized consumers, never an unrestricted
instance inventory or another consumer's links. Non-admin global session listings
require an owned sandbox and filter project-linked history using that sandbox's
project permissions; direct links remain independent.

The approval transaction selects and records one authorizing link. Prefer a
direct sandbox link when both kinds cover the sandbox; otherwise use its project
link after checking project access. Consent records the chosen link, sandbox,
bundle version, service versions, approver and fixed expiry. Retrying the same
request does not change the link, extend expiry or resurrect revoked access.
Changing link coverage requires new consent.

## GitHub connections

Keep an explicit credential-source project for a service backed by a project
GitHub connection. This is an authorization boundary, not vault ownership.
Existing GitHub services inherit their former project as that source. Connection
rows currently have no owning project column, so ownership cannot be inferred
from the connection identifier alone.

Reject a project link to a different project, and reject a direct sandbox link
unless that sandbox belongs to the credential-source project. Errors identify
the vault, service, source project and proposed consumer. Apply the same checks
when adding a service to an already linked vault or changing its GitHub source;
otherwise editing a service would bypass link-time validation. Recheck the Git
permissions against the source project at approval and during reconciliation,
along with connection validity and repository scope. Both approval and worker
enforcement require source-project `ProjectsWrite`, `SandboxesWrite` and
`GitRepositoriesRead`, even for a direct sandbox link; approval also requires
instance `GitConnectionsRead`. A narrowed project role is rejected before any
consent is issued, instead of accepting access that the worker cannot enforce. A moved sandbox loses
eligibility. Projectless sandboxes can use ordinary credentials but cannot use
these project-scoped GitHub credentials.

## Revocation and concurrency

Serialize approval and link mutation on the vault row, with a consistent lock
order of vault, services by ID, sandbox, session/runtime. Removing a link marks
it revoked and revokes the live sessions issued through that link and all their
grants in the same transaction. Another covering link never silently preserves
those sessions. Re-adding a link creates a new identity and cannot revive consent.

Vault or bundle deletion first revokes affected sessions and grants. Preserve
consent history using retirement records instead of cascading away the evidence.
Sandbox deletion must durably enqueue worker retirement before dependent rows
are removed, including direct sandbox links. Existing grant-change triggers and
the delivery reconciler propagate revocation to workers; HTTP success reports
persisted revocation, not worker acknowledgement. Failed worker delivery retries
from durable state. No new approval can race past committed revocation.

The planner joins authorizations, sessions, their selected links, services and
sandbox identities. It checks current ownership, project access when applicable,
link eligibility, policy version and expiry. It must not retain the old join
requiring service project equality with the sandbox project. Missing or revoked
links fail closed for bundle sessions. Existing project-scoped standalone grants
and assignments retain their original project authorization and deadlines; the
general APIs never expose projectless services through those legacy entry points.

Project identity on grants, runtimes and authorizations becomes optional consumer
context. It is not replaced with a fake project or zero. Existing project-backed
worker scopes retain their encoding; projectless bindings use a distinct sandbox
scope and remain pinned to sandbox ID, incarnation and worker identity. Composite
foreign keys containing a nullable project must be replaced with constraints that
still enforce grant/runtime/sandbox identity when project is absent.

## Migration and rollback

Use a new migration; do not edit shipped migrations. Create a link for each
existing vault's former project before startup exposes the general model. The DDL
transaction creates a durable backfill cursor, then commits. Both migration
runners insert legacy links in batches of at most 1,000 vaults, committing links
and cursor together outside the schema-locking transaction. Interrupted startup
resumes from that cursor even when no DDL remains pending. Completion removes
the cursor row permanently, so later starts cannot recreate deleted links.
Resolve existing sessions to that link using a constant-default legacy marker
and the deterministic legacy link ID, without rewriting historical session rows
under the schema lock or changing IDs, consent payloads, TTLs,
revocation timestamps, grants or worker incarnations. Preserve existing service
and bundle membership. Existing standalone services and direct grants retain
their restricted legacy authorization until explicitly adopted by an admin;
the migration never broadens their audience or mints new consent. New legacy
standalone approvals are rejected for services contained in a vault: callers
must approve a bundle session through a covering link. Pre-existing durable
approvals remain visible and revocable on the sandbox page and keep their
original renewal scope.

Retain former-project columns as nullable historical metadata during transition,
remove cascading ownership semantics, and introduce independent vault foreign
keys. PostgreSQL's constant default preserves legacy provenance without a table
rewrite; newly issued sessions store an explicit link and never use the legacy
fallback. The two consumer-identity constraint indexes are prepared concurrently
before upgrade DDL; the migration refuses unprepared populated tables. Fresh,
physically empty tables can create them transactionally. Non-constraint history
indexes build concurrently through post-migration maintenance. Both paths keep
retained-history index scans outside the migration transaction. Replacement
foreign keys enforce new writes immediately with `NOT VALID`; post-migration
maintenance validates the already constrained history under locks that allow
writes, rather than rescanning it under the DDL transaction's exclusive locks. Removing the legacy link still
revokes its sessions transactionally. Refuse a downgrade that cannot
represent the links, projectless consumers or retained authorization history.
Empty databases may roll back; any credential or authorization data makes the
downgrade refuse. Never silently discard general resources to restore the old
schema. Binary
rollback must fail closed for newly generalized records; verify the schema guard
rather than claiming the old binary can serve them.

## API, UI and operational bounds

The console has a top-level Vaults page and vault details containing services,
bundles and a Linked to section. Managers get explicit link/unlink controls with
revocation consequences. Project pages show linked vaults; sandbox pages show
available vaults and a Grant access action. Empty states explain missing links
and deep-link to vault setup. A typed capability response distinguishes missing
configuration from permission denial.

Handlers authorize and audit; services own all queries and business rules.
Every successful write records its actor, operation, resource ID and applicable
project context without credential values. Link and session IDs retain their
consumer and vault provenance in storage. Errors carry resource IDs
and operations and map exhaustively to Problem Details. Lists default to 20,
cap at 100 and sort by creation time descending with an ID tie-breaker. Joined
reads resolve coverage without per-row queries. No database call is added to the
worker traffic hot path. Retain existing 16-grant and 128-binding worker limits;
reject excess admission. Revocations use set-based transactional updates and the
existing bounded worker delivery queue.

## AI chat requests

After configuration, the assistant can discover eligible bundles and propose a
session for a sandbox, including a projectless sandbox. The existing pending
write action flow presents the exact sandbox, bundle and service versions and
expiry for confirmation inside chat. An administrator can also propose and
explicitly confirm a missing link in chat;
Auto mode cannot create links or approve sessions. Non-admin link proposals fail
the same management guard. No Vault page visit is required. Only a
human-confirmed pending action receives the in-process confirmation marker;
ordinary assistant API replay cannot approve access. Current permissions, link
coverage, sandbox ownership and immutable consent are rechecked at execution.
The assistant cannot call the confirmation endpoint itself or receive secrets.

## Required verification before acceptance

Exercise migration on populated storage and prove existing consent and grants
survive unchanged. Test project and direct links, projectless consumers, denied
ownership, missing project access, management 401/403, masked secrets, GitHub
source restrictions, stale edits and concurrent unlink/approval. Test worker
revocation after unlink, bundle/vault retirement and sandbox deletion, including
delivery retry. Test project membership loss and sandbox moves during renewal.
Cover frontend onboarding, read-only project views and manager controls with
adjacent tests. Regenerate both OpenAPI clients against the changed server and
run CLI spec checks, Rust checks/tests and web typechecking/tests before marking
this ADR implemented.
