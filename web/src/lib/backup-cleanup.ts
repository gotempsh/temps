export interface RetentionCleanupFailure {
  backup_id: string
  reason: string
}

export interface RetentionCleanupReport {
  dry_run: boolean
  schedule_id: number | null
  expired: number
  deleted: number
  failed: number
  failures: RetentionCleanupFailure[]
  candidate_backup_ids: string[]
  candidate_backup_ids_truncated: boolean
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

export async function cleanupExpiredBackups(options?: {
  dryRun?: boolean
  scheduleId?: number
  expectedBackupIds?: string[]
}): Promise<RetentionCleanupReport> {
  const query = new URLSearchParams()
  if (options?.dryRun) query.set('dry_run', 'true')
  if (options?.scheduleId !== undefined) {
    query.set('schedule_id', String(options.scheduleId))
  }
  const suffix = query.size > 0 ? `?${query.toString()}` : ''
  const hasExpectedCandidates = options?.expectedBackupIds !== undefined
  const response = await fetch(`/api/backups/cleanup${suffix}`, {
    method: 'POST',
    credentials: 'include',
    headers: hasExpectedCandidates ? { 'Content-Type': 'application/json' } : undefined,
    body: hasExpectedCandidates
      ? JSON.stringify({ expected_backup_ids: options.expectedBackupIds })
      : undefined,
  })
  if (!response.ok) throw new Error(await errorDetail(response))
  return (await response.json()) as RetentionCleanupReport
}
