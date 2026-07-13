import { client } from '@/api/client/client.gen'

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

const BEARER_SECURITY = [{ scheme: 'bearer', type: 'http' }] as const

export async function deleteBackup(backupId: string): Promise<void> {
  await client.delete<unknown, unknown, true>({
    security: [...BEARER_SECURITY],
    url: '/backups/{id}',
    path: { id: backupId },
    throwOnError: true,
  })
}

export async function cleanupExpiredBackups(options?: {
  dryRun?: boolean
  scheduleId?: number
  expectedBackupIds?: string[]
}): Promise<RetentionCleanupReport> {
  const { data } = await client.post<RetentionCleanupReport, unknown, true>({
    security: [...BEARER_SECURITY],
    url: '/backups/cleanup',
    query: {
      dry_run: options?.dryRun || undefined,
      schedule_id: options?.scheduleId,
    },
    body:
      options?.expectedBackupIds === undefined
        ? undefined
        : { expected_backup_ids: options.expectedBackupIds },
    throwOnError: true,
  })
  return data
}
