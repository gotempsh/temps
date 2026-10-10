// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Info } from 'lucide-react'

import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import type { DropLeftOutPath, DropLeftOutReason } from '@/lib/drop-archive'

/** Lines shown before the list collapses into "and N more". */
const VISIBLE_PATHS = 6

const REASON_LABEL: Record<DropLeftOutReason, string> = {
  version_control: 'version control',
  dependencies: 'dependencies, installed by the build',
  environment_file: 'environment file, may hold secrets',
  os_metadata: 'OS metadata',
  private_key: 'private key or credentials',
}

function filesLabel(files: number): string {
  return files === 1 ? '' : ` (${files.toLocaleString()} files)`
}

/**
 * Says what a Drop upload left out, so a missing `.env` or `node_modules/`
 * is never a surprise, and where environment variables go instead.
 */
export function DropLeftOutNotice({
  leftOut,
  fileCount,
}: {
  leftOut: DropLeftOutPath[]
  fileCount: number
}) {
  if (leftOut.length === 0) return null
  const visible = leftOut.slice(0, VISIBLE_PATHS)
  const listedFiles = visible.reduce((sum, entry) => sum + entry.files, 0)
  const remaining = Math.max(fileCount - listedFiles, 0)
  const hasEnvironmentFile = leftOut.some(
    (entry) => entry.reason === 'environment_file'
  )
  return (
    <Alert>
      <Info className="size-4" aria-hidden="true" />
      <AlertTitle>
        Left out of the upload ({fileCount.toLocaleString()}{' '}
        {fileCount === 1 ? 'file' : 'files'})
      </AlertTitle>
      <AlertDescription className="space-y-2">
        <ul className="space-y-0.5">
          {visible.map((entry) => (
            <li key={entry.path} className="break-all">
              <span className="font-mono text-xs">{entry.path}</span>
              <span className="text-muted-foreground">
                {filesLabel(entry.files)} — {REASON_LABEL[entry.reason]}
              </span>
            </li>
          ))}
          {remaining > 0 && (
            <li className="text-muted-foreground">
              and {remaining.toLocaleString()} more{' '}
              {remaining === 1 ? 'file' : 'files'}
            </li>
          )}
        </ul>
        <p className="text-muted-foreground">
          {hasEnvironmentFile
            ? 'Add the variables your app needs under Environment variables. '
            : ''}
          Templates such as .env.example are kept.
        </p>
      </AlertDescription>
    </Alert>
  )
}
