// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { FileWarning } from 'lucide-react'

export function DropInvalidArchive({ details }: { details: string }) {
  return (
    <Alert variant="destructive" role="alert">
      <FileWarning className="size-4" />
      <AlertTitle>Invalid ZIP archive</AlertTitle>
      <AlertDescription className="space-y-3">
        <p>The selected file is not a valid or supported ZIP archive.</p>
        <p>
          Create a new ZIP from your project folder using your file manager’s
          Compress option or a ZIP tool, then choose the new archive here.
          Renaming another file to .zip does not create a ZIP archive.
        </p>
        <details>
          <summary className="cursor-pointer">Technical details</summary>
          <p className="mt-2 break-words font-mono text-xs">{details}</p>
        </details>
      </AlertDescription>
    </Alert>
  )
}
