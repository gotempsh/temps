/**
 * Resolve a file configured relative to a project's root directory into the
 * repository-relative path expected by Git provider APIs.
 *
 * Compose paths are intentionally stored relative to `project.directory`
 * because deployment runs from that directory. Live repository reads do not,
 * so they must prepend the directory. Already-prefixed legacy values are kept
 * as-is to avoid producing `apps/web/apps/web/compose.yaml`.
 */
export function repositoryFilePath(
  directory: string | null | undefined,
  filePath: string
): string {
  const root = normalizeRelativePath(directory ?? '')
  const file = normalizeRelativePath(filePath)

  if (!root || !file || file === root || file.startsWith(`${root}/`)) {
    return file
  }
  return `${root}/${file}`
}

function normalizeRelativePath(value: string): string {
  return value
    .trim()
    .replace(/\\/g, '/')
    .replace(/^\.\//, '')
    .replace(/^\/+|\/+$/g, '')
    .replace(/\/{2,}/g, '/')
}
