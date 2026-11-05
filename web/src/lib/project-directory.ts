// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** Match the root spellings accepted by normalize_project_directory on the server. */
export function isRepositoryRootDirectory(
  directory: string | undefined | null
): boolean {
  const normalized = (directory ?? '')
    .trim()
    .replace(/\\/g, '/')
    .replace(/^\/+/, '')

  return normalized === '' || normalized === '.' || normalized === './'
}

/**
 * A root directory as the server stores it (normalize_project_directory):
 * trimmed, backslashes as slashes, no leading slash. Every root spelling is
 * `./`, so a stored `.` and a typed `./` compare equal. A leading `./` on a
 * subdirectory is kept, because the server keeps it too.
 */
export function canonicalProjectDirectory(directory: string): string {
  if (isRepositoryRootDirectory(directory)) return './'
  return directory.trim().replace(/\\/g, '/').replace(/^\/+/, '')
}

/**
 * Text under the root directory field. After a refused save it is the
 * server's explanation (which names the repository, branch and the
 * directories that do exist); otherwise it says what the save will check.
 */
export function rootDirectoryHelp(
  error: string | null,
  uploadedSource: boolean,
  branch: string | null | undefined
): string {
  if (error) return error
  if (uploadedSource) return 'Relative to the root of the uploaded source.'
  const onBranch = branch ? ` on branch ${branch}` : ''
  return `Must exist in the repository${onBranch}; it is checked when you save.`
}
