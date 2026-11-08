# ADR-092: Plugin AI tools are marked API operations

**Status:** Accepted
**Date:** 2026-11-08

## Decision

External plugins declare tools with an operation-level `x-temps-ai-tool`
OpenAPI extension. The extension travels in the existing ready message's
`openapi` field; no protocol version change is required. An optional description
is capped at 500 Unicode characters, falling back to the operation summary.
Unmarked operations are excluded. IDs are `plugin.<plugin_name>.<operationId>`;
requests use the real `/x/{plugin_name}/…` router path.

Only GET is read access. A plugin may escalate GET to write; it cannot downgrade
another HTTP method to read. Writes use `temps_write` and its existing human
confirmation flow, including when invoked through ADR-038's per-turn MCP bridge.
Core curated allowlists remain unchanged.

A separate plugin index is rebuilt on ready, reload, uninstall and access changes.
Rebuilding is control-plane work proportional to installed plugins and their
operation declarations. Complete snapshots are published atomically; readers
acquire an immutable snapshot without an application mutex. No plugin process,
network request or schema expansion is added to request-path index lookup.

## Security model

Execution requires an administrator's persisted, default-off per-plugin opt-in.
Disabled declarations remain discoverable with their source and console setup
path. Revocation must be checked at execution, including confirmation of an old
proposal; retaining a snapshot must not preserve authority after revocation.
The opt-in belongs on the durable plugin actor row and resets when that identity
is replaced. Toggle writes require management permission and a Temps audit entry.

Router replay carries the actual user's AuthContext. The existing plugin proxy
strips caller-supplied identity headers and injects authenticated identity; plugin
permissions still apply. Project selector validation and auto-fill, response
bounds and secret redaction apply equally to plugin operations. Descriptions and
responses are untrusted content, never instructions or authorization. Timeout
and unavailable errors identify the plugin and operation. Every executed plugin
write also receives a Temps audit record containing actor, operation and outcome.

The console always shows the AI tools section, with an empty state when no tools
are declared. Enabling access explains read access and the confirmation boundary
for writes. Existing plugins are unaffected until they declare tools and an
administrator opts in.

## Rejected alternative

Plugin-run MCP servers would introduce a second transport, lifecycle and
identity model. Registering each tool separately also places per-tool schemas
in prompts and risks accepting plugin-claimed read/write classification.
Indexing existing endpoints preserves the generic API tools and the platform's
authorization and approval boundaries.

## Implementation

The external plugin service owns the index and republishes it after lifecycle
changes and persisted consent updates. Core callers subscribe once at startup;
ArcSwap handles publish rebuilt callers for both chat and the per-turn MCP bridge.
Each tool dispatch loads the current write caller, so new proposals within an
active turn use the latest declaration and consent.
Execution rejects revoked or replaced snapshots. The caller carries an in-process
publication guard through the router; the proxy checks it after connecting and
before sending credentials or request bytes. The connected socket pins the plugin
instance, so a suspended call cannot resume against a replacement. Pending actions persist the plugin
publication revision and require a new proposal after any plugin lifecycle or
consent publication, including a server restart. The actor ID accompanies toggle
requests to prevent a stale console from enabling a replacement installation.

The Node SDK's `createToolPlugin(plugin).tool(...)` registers a route and emits
its declaration in the existing OpenAPI ready payload. The console exposes the
catalogue and consent on each plugin page. The migration defaults existing and
new plugin actors to disabled; no operator action is required during upgrade.

Marked request bodies must declare JSON object schemas, inline or through a
component reference. Unsupported root shapes and media types fail indexing with
a contextual error instead of exposing an operation with unusable body inputs.
Composite property values use explicit JSON encoding; the endpoint validates
their constraints. Recursive SDK references are rooted in OpenAPI components.
