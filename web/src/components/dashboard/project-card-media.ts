export type ProjectCardMediaKind = 'favicon' | 'screenshot'

export interface ProjectCardMediaSource {
  kind: ProjectCardMediaKind
  src: string
}

function deploymentFaviconUrl(deploymentUrl: string): string | null {
  try {
    return new URL('/favicon.ico', deploymentUrl).toString()
  } catch {
    return null
  }
}

function screenshotUrl(location: string): string {
  return `/api/files${location.startsWith('/') ? location : `/${location}`}`
}

export function projectCardMediaSources(
  deploymentUrl?: string | null,
  screenshotLocation?: string | null
): ProjectCardMediaSource[] {
  const sources: ProjectCardMediaSource[] = []
  const favicon = deploymentUrl ? deploymentFaviconUrl(deploymentUrl) : null

  if (favicon) sources.push({ kind: 'favicon', src: favicon })
  if (screenshotLocation) {
    sources.push({ kind: 'screenshot', src: screenshotUrl(screenshotLocation) })
  }

  return sources
}
