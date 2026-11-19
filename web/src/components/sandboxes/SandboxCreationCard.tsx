// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { AlertTriangle, RefreshCw } from 'lucide-react'
import type { SandboxCreation } from '@/api/client'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { cn } from '@/lib/utils'
import { SandboxCreationSteps } from './SandboxCreationSteps'

interface SandboxCreationCardProps {
  /** The last steps read; `undefined` until the first poll answers. */
  creation: SandboxCreation | undefined
  /** Why the last read failed, or `null` when it did not. */
  refreshError: string | null
  onRetry: () => void
  retrying: boolean
}

/**
 * The steps of a sandbox that is still starting, on its detail page. A
 * failed poll keeps the last steps (and download progress) read on screen
 * with the error and a retry, instead of dropping them.
 */
export function SandboxCreationCard({
  creation,
  refreshError,
  onRetry,
  retrying,
}: SandboxCreationCardProps) {
  return (
    <Card>
      <CardHeader className="pb-3">
        <CardTitle className="text-sm font-medium">
          Creating the sandbox
        </CardTitle>
      </CardHeader>
      <CardContent className="space-y-3">
        {refreshError && (
          <Alert variant="destructive">
            <AlertTriangle className="size-4" />
            <AlertDescription className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
              <span className="[overflow-wrap:anywhere]">
                {`${creation ? 'The steps could not be refreshed' : 'The steps could not be read'}: ${refreshError}`}
              </span>
              <Button
                variant="outline"
                size="sm"
                disabled={retrying}
                onClick={onRetry}
              >
                <RefreshCw
                  className={cn('mr-1.5 size-4', retrying && 'animate-spin')}
                />
                Try again
              </Button>
            </AlertDescription>
          </Alert>
        )}
        {/* Nothing read yet and the read failed: the error says so; a
            skeleton would claim it is still loading. */}
        {(creation !== undefined || !refreshError) && (
          <SandboxCreationSteps creation={creation} />
        )}
      </CardContent>
    </Card>
  )
}
