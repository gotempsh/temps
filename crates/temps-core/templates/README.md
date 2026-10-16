# Bundled templates

Each bundled catalog entry lives in its own YAML file:

- `starters/<slug>.yaml` contains source-code project starters (`kind: starter`).
- `services/<slug>.yaml` contains curated native service templates (`kind: app`).

The file contains one `ProjectTemplate` object directly, not a catalog wrapper.
Its filename must match `slug`, and its `kind` must match the parent directory.
Nested directories are supported when a category grows, but slugs remain unique
across the complete catalog.

The Rust loader embeds every `.yaml` file recursively, orders entries by path,
and validates the assembled schema at startup and in tests. Adding a template
therefore does not require updating a Rust registry.

Operator-specific catalogs remain separate: `<data-dir>/templates.yaml` and
the `--additional-templates` files retain the versioned `TemplatesConfig`
wrapper so operators can extend or override the bundled catalog.

## Template YAML reference

### Top-level fields

| Field | Description |
|-------|-------------|
| `slug` | Unique identifier, must match the filename (without `.yaml`). |
| `name` | Display name. |
| `version` | Semver string of the template release. |
| `kind` | `app` for a single native service; `starter` for a source-code scaffold. |
| `description` | One-sentence summary shown in the catalog. |
| `git` | `{ url, ref }` - source repository and pinned tag/commit. Required for starters. Apps that build from their `image:` may still set `git` for documentation. |
| `image` | Docker image reference for apps deployed from a prebuilt image. |
| `services` | List of `TemplateServiceSpec` objects (see below). For single-service templates the list contains one item. |
| `shared_env_vars` | Variables set once on the project and inherited by all services. |
| `databases` | Managed database types the template needs (e.g. `[postgres]`). |
| `routes` | Path-routing rules between services (multi-service templates only). |

### `services:` definitions

Each entry in `services:` describes one deployable service:

```yaml
services:
  - name: backend         # identifier within this template
    directory: api        # path within the repository (`.` for root)
    preset: dockerfile    # build system; see VALID_PRESETS in templates.rs
    git:                  # optional: pin a per-service git source
      url: https://github.com/example/service.git
      ref: v1.0.0
    image: ghcr.io/example/svc:1.0.0   # prebuilt image (skips build)
    exposed_port: 8080
    env_vars:
      - name: SECRET_KEY
        secret: true
        required: true
        default_generator: random_secret
```

For multi-service templates all services should pin a `git.ref` (or use `image:`)
so the exact version deployed is reproducible. The top-level `git.ref` acts as a
fallback when a service does not declare its own `git:`.

### Routes (multi-service)

Path routing between services in the same project:

```yaml
routes:
  - source: frontend     # service that receives the request
    prefix: /api          # path prefix forwarded to target
    target: backend
    strip_prefix: false   # whether to remove the prefix before forwarding
```
