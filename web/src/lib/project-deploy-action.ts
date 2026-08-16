import type { SourceType } from '@/api/client'

export function projectDeployLaunchMode(
  sourceType: SourceType
): 'dialog' | 'upload' {
  return sourceType === 'uploaded_source' || sourceType === 'static_files'
    ? 'upload'
    : 'dialog'
}

export function deploymentsAfterStartPath(projectSlug: string): string {
  return `/projects/${projectSlug}/deployments?autoRefresh=true`
}
