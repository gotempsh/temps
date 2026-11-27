---
name: temps-plugin
description: >
  Design, build, test, and distribute external Temps plugins with TypeScript/Bun;
  provide development and local-testing guidance for existing Rust plugins.
  Use for creating a plugin, adding an embedded console UI or deployment-event automation,
  testing host permissions and AI, deploying it as a Temps service, or submitting a plugin to the
  public catalog. End-to-end publishing covers TypeScript GitHub-source plugins, not Rust
  native-package releases or in-process TempsPlugin backend crates.
---

# Create and ship a Temps plugin

For TypeScript/Bun, deliver a working external plugin, a deployable source repository, and evidence of
what was verified. Continue an existing project when provided; preserve its identity,
storage, and user changes. Publishing is a separate outcome from building or deploying.

Rust support here is limited to development and local simulator testing. Rust distribution
requires the separate signed native npm-package/catalog pipeline, which this skill does
not teach. A plugin runs only as a Temps service: do not suggest copying/symlinking a
binary into the host plugin directory; Temps does not start plugin executables. If Rust publication is
requested, establish the supported native release procedure before claiming an end-to-end
plan; do not silently change the user's implementation language.

## 1. Establish the contract

Record the plugin's purpose, intended user, entry point in Temps, input/output, persisted
data, required host permissions, resource limits, and supported host/SDK versions. Resolve
only missing decisions that affect the implementation. Prefer TypeScript + Bun for a new
plugin; retain Rust for existing Rust plugins or an explicit choice.

Read applicable repository instructions. Inspect the installed CLI's `plugin --help`
and the SDK's exports instead of assuming commands from another release exist.
The canonical TypeScript starter is
[gotempsh/temps-plugin-template](https://github.com/gotempsh/temps-plugin-template).
Use its current README, manifest, lockfile, and CI. The template may evolve ahead of the
npm CLI; lack of a CLI command is not a reason to invent one.

Current evidence sources in a Temps checkout:

- `apps/temps-cli/src/commands/plugin/`: init, build, services, add-service, remove, grants and dev simulator.
- `crates/temps-external-plugins/src/service_runtime.rs`: how Temps attaches to a plugin service.
- `docs/adr/119-a-plugin-is-a-temps-service.md`: the runtime contract.
- `crates/temps-plugin-protocol/`: protocol, permissions, manifest and events.
- `sdks/node/packages/plugin-sdk/src/` and `crates/temps-plugin-sdk/src/`: SDK APIs.
- Root `DESIGN.md`, `web/src/components/ui/`, and `web/src/globals.css`: current console design.

Read [development.md](references/development.md) when scaffolding or changing runtime
behavior, [design.md](references/design.md) for UI work, and
[publishing.md](references/publishing.md) before preparing a distributable repository.

## 2. Scaffold an independently buildable project

Use the GitHub template or, when supported by the installed CLI:

```sh
bunx --bun @temps-sdk/cli plugin init my-plugin --name @your-scope/my-plugin
```

This command scaffolds local files; it does not publish. Inspect generated files before
continuing. Replace placeholder identity, author, repository, version and description;
keep package metadata and runtime manifest consistent. Add only tested platforms.
Commit the lockfile. Keep credentials, local databases, logs, auth material, and simulator
session files out of Git.

The repository is deployed as a Temps service, from its root or from a self-contained
plugin subdirectory set as the service's root directory. Verify that a clean checkout
builds through the ordinary deployment path. See the publishing reference for the
embedded-asset consequence.

## 3. Build the product

Use the SDK to own the protocol handshake and transport. Keep protocol stdout clean;
log diagnostics to stderr. Use verified caller context for authorization, typed errors,
and plugin-owned storage in its assigned data directory. Host data and host-brokered AI
come through the SDK, never database/provider credentials copied from the host.

Declare the minimum host permissions and handle absent configuration and revoked grants
visibly. Manifest declarations request permissions; administrators grant them. The plugin
runs in its service's container and reaches Temps only through the platform API with
those grants; it is never given the control-plane database or the instance data directory.

Register discoverable navigation for a UI plugin. Use the selected Temps design standard,
embed assets, and test its real proxy path. For deployment automation, subscribe to
`deployment.succeeded`, keep receipt handling quick, and use bounded persistent jobs,
idempotency, cancellation and explicit environment/project settings.

## 4. Verify the TypeScript lifecycle at three levels

1. **Unit and integration checks:** success, validation, failures, persistence/restart,
   bounded work, and relevant security cases. Typecheck and compile the actual binary.
2. **Local plugin simulator:** real binary + SDK protocol + mock host. Exercise sidebar UI,
   event delivery, duplicates, denied/granted/revoked permissions, and role restrictions.
3. **Temps instance:** deploy a reviewed repository revision as a service, add it as a
   plugin, open it through Temps, trigger the intended user workflow, change permissions,
   and redeploy the same service. Verify persistence and relevant actor/audit attribution where supported.

Read [development.md](references/development.md) for simulator commands. A standalone
unauthenticated UI preview proves layout only. Simulator/mock AI results are not real
provider calls or a real Temps instance. A received event is not proof the job finished.
Do not label end-to-end verification complete unless the deployed workflow was exercised;
record a concrete limitation when that environment is unavailable.

## 5. Distribute, list, and maintain TypeScript plugins

The default workflow is **GitHub repository → deployed as a Temps service → catalog PR**.
No npm publication, cloud publisher token, or registry signing key is needed for this path.
Read [publishing.md](references/publishing.md) for exact metadata and deployment examples.
`plugin publish` is a separate native npm/signed-catalog flow, not shorthand for listing a
GitHub repository; use it only when that distribution channel is explicitly requested.

Prepare and validate all artifacts before an external publication step. Respect the
user's existing authorization: creating a skill or building a plugin does not itself
request public npm publication, a release tag, or deployment into production. When
publication is requested, show the concrete repository/version/permissions and execute
only that scope. Never ask users to paste credentials into chat.

Finish with source location, preview/deployment instructions, declared permissions,
verification evidence, distribution/catalog state and remaining limitations. For updates,
retain the stable identity, version deliberately, migrate storage safely, and test a
redeploy of the same service. A plugin changes when its service is redeployed.
