/**
 * The canonical on-disk shape of `apps/temps-cli/openapi.json`.
 *
 * The CLI's client is generated from a *committed* copy of the spec, unlike
 * `web/`, which generates straight from a running server. That makes the file
 * a reviewable artifact — and it only stays reviewable if its shape is a
 * function of the API alone.
 *
 * Two things break that:
 *
 *   - The server serves the document minified on one line. Writing that
 *     straight to disk turns ~92,000 lines into 1, so the pull request reports
 *     ~92,000 deletions and the real change is invisible.
 *   - Key order comes from serde and is not stable between builds, so even a
 *     pretty-printed dump reorders large unrelated blocks.
 *
 * Sorting keys recursively removes both. Shared by `spec:update` (which
 * fetches) and `spec:check` (which verifies), so the writer and the gate can
 * never disagree about what canonical means.
 */

export const SPEC_PATH = new URL('../openapi.json', import.meta.url).pathname

/** Recursively sort object keys so on-disk order never depends on the server. */
export function canonicalize(value: unknown): unknown {
  if (Array.isArray(value)) {
    // Array order is meaningful in OpenAPI (parameter lists, enum values) —
    // sort the contents, never the sequence.
    return value.map(canonicalize)
  }
  if (value && typeof value === 'object') {
    const source = value as Record<string, unknown>
    return Object.fromEntries(
      Object.keys(source)
        .sort()
        .map((key) => [key, canonicalize(source[key])])
    )
  }
  return value
}

/** Canonical text for a parsed spec: sorted keys, two-space indent, trailing newline. */
export function serialize(spec: unknown): string {
  return `${JSON.stringify(canonicalize(spec), null, 2)}\n`
}

/**
 * How many paths a parsed document declares; `0` for anything unusable.
 *
 * A spec with no paths means something answered but the document was never
 * assembled. Writing that would silently delete the entire committed client
 * and the generated SDK would compile to nothing, so both the writer and the
 * checker treat zero as fatal.
 */
export function pathCount(spec: unknown): number {
  const paths = (spec as { paths?: unknown } | null)?.paths
  if (!paths || typeof paths !== 'object') {
    return 0
  }
  return Object.keys(paths).length
}
