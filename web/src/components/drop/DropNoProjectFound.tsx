// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { FileQuestion } from 'lucide-react'

/**
 * Shown when Drop found nothing it can build in the selected files.
 *
 * Detecting the same files again cannot succeed, so this replaces the retry
 * with what Temps looks for and the change that always works; the page's
 * primary action switches to choosing different files.
 */
export function DropNoProjectFound({
  supportedFiles,
}: {
  supportedFiles: string[]
}) {
  return (
    <Alert role="alert">
      <FileQuestion className="size-4" />
      <AlertTitle>
        Nothing in these files tells Temps how to build them
      </AlertTitle>
      <AlertDescription className="space-y-3">
        <p>
          Add a <code className="font-mono">Dockerfile</code> at the root of
          your project to deploy any application, or add the manifest your
          language uses, then drop the files again.
        </p>
        {supportedFiles.length > 0 && (
          <div>
            <p className="mb-1.5 text-xs text-muted-foreground">
              Temps recognises:
            </p>
            <ul className="flex flex-wrap gap-1.5">
              {supportedFiles.map((name) => (
                <li
                  key={name}
                  className="rounded-md border bg-muted/50 px-1.5 py-0.5 font-mono text-xs"
                >
                  {name}
                </li>
              ))}
            </ul>
          </div>
        )}
      </AlertDescription>
    </Alert>
  )
}
