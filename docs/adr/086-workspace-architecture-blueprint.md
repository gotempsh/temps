# ADR-086: Workspaces hold the chats, and an accepted architecture blueprint

> **Renumbered:** this ADR was numbered 053 until 2026-10-31, when duplicate ADR numbers were removed. Older references to "ADR-053" may mean this document; the [ADR index](./README.md#renumbered-adrs) lists every renumbered ADR.

**Status:** Accepted (steps 1 to 5 implemented)
**Date:** 2026-10-20
**Author:** David Viejo
**Builds on:** ADR-036 (persistent workspace sandboxes), ADR-084 (sandbox service access)

---

## Context

Temps has two places where an AI builds software, and they grew apart.

| | Assistant chats | AI workspaces (`ai_applications`) |
|---|---|---|
| Entry point | the assistant dock and full-page chat (`AssistantPanels`) | `/ai-first` ("AI workspace" in the sidebar) |
| Unit | one conversation (`context_type = 'assistant'`) | one application holding many threads (`context_type = 'application'`) |
| Model | any AI-gateway model, native function calling | a host CLI harness only (Claude Code, Codex) |
| Sandbox | one per chat, linked through `ai_conversations.metadata` (`sandbox_link.rs`) | one per application, shared by every thread (`ai_application_workspaces`) |
| Projects | whatever the chat's confirmed writes created (`chat_resources.rs`) | linked to the application (`ai_application_projects`) |
| Plan of the app | none | `topology` artifacts: written by `render_ui`, untyped, append-only, owned by one thread, drawn as a flat list |

Building a real application exposes three gaps:

1. **The chat is the wrong unit.** An app with a front end, an API, a worker and
   a database takes several conversations: one to build, one to fix a bug, one
   to add a feature. Each assistant chat starts from nothing, with its own
   sandbox and its own list of what it made.
2. **Nothing holds the design.** Neither the user nor the model has a shared,
   agreed picture of the parts, how data flows between them, what is
   published where and which database holds what. The model drifts: it adds a
   service nobody asked for, or forgets the worker it planned two turns ago.
3. **The side panel shows history instead of the app.** Every deployment a
   goal ever watched becomes its own tab (`watchedDeployments`), so a build chat
   collects `taskhive · #41`, `#43`, `#44`… and still shows no picture of what
   the application is.

## Decision

### 1. A workspace is the unit; a chat is a thread in it

The existing `ai_applications` row is the workspace (the console calls it an
"application" in the chat). It owns the linked projects and services and the
blueprint, and its chats share one sandbox.

- **Assistant chats join workspaces** through `ai_conversations.application_id`
  and keep `context_type = 'assistant'`, their gateway model and the API tools.
  Only assistant chats with no `project_id` can join:
  `chk_ai_conversations_single_context` allows one of the two, and a chat about
  one project is scoped to that project. `GET /ai/conversations/{id}/workspace`
  says so (`joinable: false` with a `reason`) rather than leaving the console to
  infer it from an error.
- **Questions stay light.** A chat that only asks needs no workspace. A chat
  gets one when the assistant proposes an architecture for it (the propose tool
  creates the workspace, named after the app, and records it in the result) or
  when the user creates or picks one from the chat's application chip. A chat
  can be moved to another workspace or taken out of it.
- **One sandbox per workspace.** The sandbox link and preview port stay in each
  chat's metadata, but writing either key now writes it for every assistant
  chat of the same owner in the same workspace (`metadata_scope` in
  `sandbox_link.rs`). Joining copies the sandbox of the chat that should win to
  the rest: `keep_sandbox = workspace` (the default when joining an existing
  one) keeps the workspace's, `chat` brings the joining chat's. Leaving removes
  the leaver's own copy of the keys and touches no sibling.

### 2. Each workspace has one blueprint, versioned and accepted by the user

The blueprint is a typed description of the application: its parts, and the
flows of data between them. It is not code and not a deployment plan; it is
the agreement the user and the model build against.

```rust
pub struct BlueprintDocument {
    /// At most three sentences: what the application does and for whom.
    pub summary: String,
    pub components: Vec<Component>,
    pub flows: Vec<Flow>,
}

/// Flat, with the fields of the other kinds absent: a tagged union is harder
/// for a model to fill reliably through a JSON schema, and the validator
/// rejects a field that belongs to another kind by name.
pub struct Component {
    pub id: String,          // stable slug: `web`, `api`, `orders-worker`
    pub name: String,
    pub purpose: String,     // why it exists, in one sentence
    pub kind: ComponentKind, // client | app | data | external
    pub surface: Option<ClientSurface>, // client: browser | mobile | cli | third_party
    pub role: Option<AppRole>,          // app: web | api | worker | cron | static
    pub stack: Option<String>,          // app: free text, "Bun + Hono", "FastAPI"
    pub runtime: Option<AppRuntime>,    // app: preset | dockerfile | image
    pub image: Option<String>,          // app running an image: "nats:2.10"
    pub engine: Option<DataEngine>,     // data: postgres | redis | mongodb | mariadb | s3
    pub holds: Vec<String>,             // data: tables, key prefixes, buckets, streams
    pub provider: Option<String>,       // external: "Stripe", "an LLM API"
}

pub struct Flow {
    pub from: String,
    pub to: String,
    pub kind: FlowKind, // http | websocket | query | publish | subscribe | cache | blob | webhook
    /// What travels: "POST /api/tasks", "topic task.created", "tables tasks".
    pub label: String,
}
```

The shape is open where the user decides (any stack, any number of parts, any
topics and routes) and closed where Temps must act (an app runs by preset,
Dockerfile or image; data lives in an engine Temps operates). A broker is not a
separate kind: pub/sub over Redis Streams is a Redis data part with
`publish`/`subscribe` flows, and a dedicated broker is an app running an image.

**Validation** (server side, every problem listed at once, up to 20, each
naming the part or flow): ids are unique lowercase slugs; each kind has its
required fields and none of another kind's; every flow names existing parts,
never starts at data and never ends at a client; `query` ends at Postgres,
MariaDB or MongoDB, `cache` at Redis, `blob` at S3, `publish`/`subscribe` at a
Redis part or an app; no value looks like a credential or a URL with one
(`validate_secret_free_payload`); and it is bounded: 40 parts, 120 flows, 2,000
characters of summary, 200 per label.

**Storage.** `ai_blueprints` (one per application: `application_id` unique,
`accepted_version`, and `bindings`, a JSON map from part id to
`{project_id?, service_id?}`) and `ai_blueprint_versions` (`blueprint_id`,
`version`, `document` JSONB, `change_summary`, `status` of `proposed |
accepted | rejected | superseded`, `proposed_in` conversation, `created_by`,
`decided_by`, `decided_at`). Bindings live on the blueprint, not in the
document, so binding a project never creates a version. Versions are
immutable; deciding one only changes its status. Writes lock the blueprint row,
so at most one version is `proposed`: a new proposal supersedes the open one,
and a version the user saves is accepted at once and supersedes both.

**Live state is read, never stored.** On read, each binding resolves to
`created | building | live | failed | stopped | missing` from the bound
project's latest deployment (one `DISTINCT ON` query for all of them) or the
service's status, in three queries whatever the size of the blueprint.

### 3. The model proposes; the user accepts

- **`temps_propose_blueprint { document, change_summary, workspace_name? }`**
  validates and stores a proposed version (creating the chat's workspace first
  when it has none) and returns it. The chat renders the result as a card: the
  diagram with what is added or changed highlighted against the accepted
  version, the change summary, and **Accept and build** / **Change something**.
  Deciding is a user action through the API, audited, never something the model
  can do. After the decision the console sends the chat a message ("I accepted
  the architecture (version 2). Build it as designed.") so the user does not
  have to type it.
- **Proposing ends the building for that turn.** Once a turn has stored a
  proposal, every later tool call in the same turn except reading the platform,
  reading a playbook, reading a file and proposing again is refused with an
  explanation, so a model that ignores "stop and wait" cannot build an
  unaccepted design.
- **The accepted blueprint is in every turn's context**, as a compact text
  rendering (parts with purpose, stack and binding, then flows), in every chat
  of the workspace, with the rules for building against it. A waiting proposal
  is mentioned so the model does not build it.
- **`temps_bind_blueprint { component_id, project_id? | service_id? }`** records
  which project or service realises a part, after checking the caller can see
  it, and links the project to the workspace.
- **The user can redraw it.** The Architecture tab's editor saves a version
  through `POST .../blueprint/versions`; it is validated like a proposal and
  accepted at once, and the chat is told what changed.

### 4. The workspace panel leads with the architecture

The working tabs' first tab is **Architecture**: the accepted blueprint, with
an open proposal above it.

- Layout is computed: columns in the order data travels (clients, apps by
  role, data, outside services), curved SVG edges labelled with the flow kind,
  publish/subscribe dashed. No graph library. A container narrower than 600px
  gets the same content as a list, so the docked panel stays readable.
- Each bound part shows its live status; selecting one shows its purpose,
  stack, status and a link to its project or service.
- A chat with no design shows what the tab is for with a concrete example,
  **Ask the assistant to design it** and **Draw it myself**; a project chat
  says why it has no architecture.
- **Deploy all** asks the chat to deploy every app part from the sandbox in
  dependency order (an app another app calls goes first). It goes through the
  assistant on purpose: the default source type is an uploaded sandbox
  bundle, which the console cannot redeploy without a fresh upload, and the
  assistant's deploy tool already packs, asks for approval and binds.
- A proposal the chat has not shown yet opens the panel on this tab.
- **Deployments are one tab**, newest first, with what is live marked.

### API

| Method | Path | Purpose |
|---|---|---|
| `GET` | `/ai/conversations/{id}/workspace` | the chat's workspace, its other chats, whether it can join one |
| `PUT` | `/ai/conversations/{id}/workspace` | join an existing workspace or create one (audited) |
| `DELETE` | `/ai/conversations/{id}/workspace` | take the chat out of its workspace (audited) |
| `GET` | `/ai/applications/{id}/blueprint` | accepted version, open proposal, bindings, live state |
| `GET` | `/ai/applications/{id}/blueprint/versions` | history, paginated (20 / 100), newest first |
| `POST` | `/ai/applications/{id}/blueprint/versions` | save the user's own version, accepted at once (audited) |
| `POST` | `/ai/applications/{id}/blueprint/versions/{version}/accept` | accept a proposal (audited) |
| `POST` | `/ai/applications/{id}/blueprint/versions/{version}/reject` | reject a proposal (audited) |
| `PUT` | `/ai/applications/{id}/blueprint/bindings/{component}` | bind a part to a project or service (audited) |
| `DELETE` | `/ai/applications/{id}/blueprint/bindings/{component}` | remove a binding (audited) |

Model proposals come only from the tool, so a proposed version always records
the chat that proposed it. Access follows the application's own checks; every
route is control plane. These operations are excluded from the assistant's
`temps` tool: they are the chat's own machinery, reached through the two
dedicated tools.

## Consequences

- One mental model: a workspace is an application being built, its chats are
  conversations about it, and the blueprint is what everyone agreed it is.
- The model stays on course because the agreed design is in every turn, and a
  change of design is an explicit, reviewable step rather than drift.
- Resources, sandbox and deployments are found where they belong, under the
  part of the app they serve, not in a growing row of tabs.
- Assistant chats gain a scope they did not have. Moving a chat into a
  workspace that already has a sandbox keeps one of the two (`keep_sandbox`);
  the other sandbox is no longer linked but is not deleted. Once no chat links
  to it, the service connections it was granted (ADR-051) are released, as
  they are when a chat unlinks a sandbox; a chat leaving a workspace does the
  same for a sandbox it was the last to use.
- `topology` artifacts become redundant; existing ones keep rendering as they do
  and the model stops writing them once blueprints exist.

## Alternatives considered

- **A blueprint per chat.** Rejected: the design would be lost the moment work
  moves to a second chat, which is exactly gap 1.
- **Reuse the `topology` artifact.** Rejected: artifacts belong to one thread,
  are append-only, carry no schema and cannot be accepted.
- **A new workspace concept beside `ai_applications`.** Rejected: it would be a
  third model of the same idea.
- **A free-form diagram (Mermaid text, or nodes the model positions).**
  Rejected: it cannot be validated, bound to projects and services, or diffed
  between versions.
- **A graph library such as React Flow.** Deferred: dragging is not needed for a
  computed layered layout, and it can be added if direct editing arrives.

## Plan

1. **Deployments tab.** Done.
2. **Assistant chats in workspaces.** Done: join, create, move and leave, one
   shared sandbox, the workspace on each chat in the list.
3. **Blueprint.** Done: schema, tables, validation, the propose tool, the
   proposal card, the Architecture tab and the blueprint in every turn.
4. **Live binding.** Done: the bind tool, live status, Deploy all through the
   chat.
5. **Direct editing.** Done as a form editor (parts, kinds, flows) that saves a
   new accepted version.

Building an accepted design in one turn needs more steps than a conversational
turn did: an assistant turn may take 40 tool rounds, and three rounds before
the cap the model is told to start nothing new, set a `temps_goal` on anything
still running (a deployment building) and report where things stand. When a
goal on a deployment is reached, the goal watcher starts one short turn in the
chat to check the live app (its routes from the sandbox when the sandbox can
reach them, and its container logs) and report what it saw, since a completed
deployment has only passed the health check on `/`.

Not done: running CLI harness threads in the same workspace as gateway-model
chats, and retiring `topology` artifacts.
