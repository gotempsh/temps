# Bundled templates

Each bundled catalog entry lives in its own YAML file:

- `starters/<slug>.yaml` contains source-code project starters.
- `services/<slug>.yaml` contains curated native service templates.

The file contains one `ProjectTemplate` object directly, not a catalog wrapper.
Its filename must match `slug`, and its `kind` must match the parent directory.
Nested directories are supported when a category grows, but slugs remain unique
across the complete catalog.

The Rust loader embeds every `.yaml` file recursively, orders entries by path,
and validates the assembled schema at startup and in tests. Adding a template
therefore does not require updating a Rust registry.

Operator-specific catalogs remain separate: `<data-dir>/templates.yaml` and
the `--additional-templates` files retain the versioned `TemplatesConfig`
wrapper so operators can override or extend the bundled catalog.
