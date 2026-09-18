# Source distribution and catalog listing

Prefer the current GitHub-source workflow for new TypeScript plugins. Verify the target
host supports it; older releases may only support binary/signed-catalog installation.
Current authoritative instructions:

- [TypeScript template](https://github.com/gotempsh/temps-plugin-template)
- [Catalog contribution guide](https://github.com/gotempsh/plugins/blob/main/registry/README.md)
- Host installer: `crates/temps-external-plugins/src/repository.rs`

## 1. Prepare a dedicated repository

Create from the template when repository creation is in scope. If it already exists, work
there. The repository root needs a nonempty `README.md`, `package.json`, committed Bun
lockfile, and the source/assets necessary to compile its entrypoint. Neither catalog
subdirectory paths nor installing a plugin folder inside another repository are currently
supported. Extract a monorepo example to its own repository before presenting it as
GitHub-installable.

Keep the template's current metadata shape. Example fields to customize:

```json
{
  "name": "@your-scope/route-checker",
  "version": "0.1.0",
  "private": true,
  "description": "Find broken routes after deployment",
  "author": "Your team",
  "repository": "https://github.com/your-org/route-checker",
  "temps": {
    "name": "route-checker",
    "title": "Route Checker",
    "category": "SEO",
    "entrypoint": "src/index.ts",
    "platforms": ["linux-amd64-gnu"],
    "screenshots": [
      {"path": "assets/plugin-light.png", "alt": "Route Checker showing a completed crawl"}
    ]
  }
}
```

Merge these fields into the real package, retaining dependencies and scripts. Scoped npm
naming does not require npm publication. Runtime manifest identity/version must match.
Advertise only compatible tested targets. README covers purpose, prerequisites, permissions,
installation, configuration, examples, resource limits, data retention, update/uninstall,
license and support. Add real screenshot files before referencing them.

## 2. Reproduce the installation build

A local `bun run build` is insufficient evidence. The current host installs locked
dependencies with lifecycle scripts disabled, prefetches the target Bun runtime, then
compiles offline with an installer-owned command. It does not invoke your package's build
script. Inspect the target host's code: the reviewed implementation compiles `src/index.ts`.
Keep the manifest entrypoint there unless that host explicitly supports another path.

In a fresh temporary checkout of the exact proposed commit:

```sh
bun install --frozen-lockfile --ignore-scripts
bun build --compile src/index.ts --outfile /tmp/route-checker-validation
```

Use a unique temporary output path for actual runs. This local command tests clean-checkout
completeness; it is not a substitute for the host's container/platform test. Missing
`web/dist` or a generated asset module is a release blocker. Generate and commit the needed
UI assets (or use a self-contained source embedding approach) and verify reproducible
regeneration. Do not rely on ignored files left by a developer's prior UI build.

Run the normal tests/typecheck/build and CI as well. Do not introduce install-time scripts
requiring secrets or network access during compilation. Building a repository executes
its code/dependencies and still requires source review.

## 3. Install and test in Temps

Choose **Plugins → Install from GitHub**, paste the repository URL, review trust and
requested permissions, then install. The server needs Git and a working Docker daemon;
it builds the native executable. A public repository needs no GitHub credential. Private
repository access must be configured on the **host**, not just the author's workstation.
Do not embed credentials in the repository URL.

If the configured CLI supports these commands:

```sh
bunx --bun @temps-sdk/cli plugin install https://github.com/your-org/route-checker --ref <commit-sha> --grant events_read
bunx --bun @temps-sdk/cli plugin update route-checker --ref <new-commit-sha>
```

Use actual immutable SHAs, not the literal placeholders. Grant only permissions declared
and used by the plugin. Omit `--grant` for a plugin that needs none. Inspect `--help` for
AI quota options when needed. Avoid `--yes` unless source trust has already been authorized.
A declaration does not self-approve a grant. Installation/update refreshes the plugin;
restarting the entire Temps server is not normally required. After CLI installation,
return to the console tab to refresh its plugin list.

Verify sidebar entry, real host API behavior, denied/revoked permissions, persisted data,
intended deployment events and same-source upgrade. Uninstall/reinstall or a different
source can rotate actor identity and approvals; do not use that as an update shortcut.

## 4. Submit to the discovery catalog

A working public GitHub repository can be shared directly before it has a listing.
To request discovery in the public catalog, open a PR to `gotempsh/plugins` adding:

`registry/route-checker.json`

```json
{
  "repo": "your-org/route-checker",
  "categories": ["seo"]
}
```

The filename matches `temps.name`. Current categories are `analytics`, `automation`,
`databases`, `developer-tools`, `observability`, `seo`, `security`, `other`; the first is
primary. Check the live contribution guide for changes. Submit the small repository record,
not a hand-edited generated `registry/catalog.json`. The generator reads metadata and
screenshots at a pinned source commit. Run the catalog repository's documented validation
and include install/runtime evidence in the PR.

Catalog validation installs dependencies without lifecycle scripts and compiles in a capped
offline container after dependency resolution. A build-validation pass is not a runtime
or security audit. A listing is discovery metadata, not blanket trust in executable code.
After merge, confirm the generated catalog includes the intended SHA and the registry/host
listing refreshes; do not promise an immediate update or auto-update installed instances.
For subsequent releases, bump version, test, push a reviewed commit and follow the current
catalog refresh process. Existing installations require an explicit update action.

## Other channels and publication status

The CLI may still expose `plugin publish`, which builds public npm native packages and
submits to a separate signed-catalog workflow. Do not call it for GitHub-source listing.
Its tokens, signing pipeline and retry semantics are separate; consult matching release
instructions only when that channel is explicitly requested. Plugin authors never need
the registry's private signing keys.

Report each status separately: source ready, clean-checkout build passed, installed and
verified, catalog PR opened, catalog merged/listed. Do not call the plugin published when
only a local binary or PR exists. Preparing this workflow does not authorize deploying to
production or publishing packages on the user's behalf.
