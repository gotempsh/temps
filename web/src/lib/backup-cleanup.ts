export interface RetentionCleanupFailure {
  backup_id: string
  reason: string
}

export interface RetentionCleanupReport {
  expired: number
  deleted: number
  failed: number
  failures: RetentionCleanupFailure[]
}

async function errorDetail(response: Response): Promise<string> {
  try {
    const body = (await response.json()) as { detail?: string; title?: string }
    return body.detail ?? body.title ?? response.statusText
  } catch {
    return response.statusText
  }
}

export async function deleteBackup(backupId: string): Promise<void> {
  const response = await fetch(`/api/backups/${encodeURIComponent(backupId)}`, {
    method: 'DELETE',
    credentials: 'include',
  })
  if (!response.ok) throw new Error(await errorDetail(response))
}

export async function cleanupExpiredBackups(): Promise<RetentionCleanupReport> {
  const response = await fetch('/api/backups/cleanup', {
    method: 'POST',
    credentials: 'include',
  })
  if (!response.ok) throw new Error(await errorDetail(response))
  return (await response.json()) as RetentionCleanupReport
}
