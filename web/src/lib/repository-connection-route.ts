export function repositoryConnectionBasePath(projectSlug: string): string {
  return `/projects/${encodeURIComponent(projectSlug)}/connect-repository`
}

export function repositoryConnectionPath(
  projectSlug: string,
  connectionId: number
): string {
  return `${repositoryConnectionBasePath(projectSlug)}/connections/${connectionId}`
}

export function repositorySelectionPath(
  projectSlug: string,
  connectionId: number,
  repositoryId: number
): string {
  return `${repositoryConnectionPath(projectSlug, connectionId)}/repositories/${repositoryId}`
}
