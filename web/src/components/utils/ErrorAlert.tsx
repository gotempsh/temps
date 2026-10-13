// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { AlertCircle } from 'lucide-react'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'

interface ErrorAlertProps {
  title?: string
  description: string
  retry?: () => void
  /** True while the retry is in flight: the button says so and ignores clicks. */
  retrying?: boolean
}

export function ErrorAlert({
  title = 'Error',
  description,
  retry,
  retrying = false,
}: ErrorAlertProps) {
  return (
    <Alert variant="destructive">
      <AlertCircle className="h-4 w-4" />
      <AlertTitle>{title}</AlertTitle>
      <div className="flex items-center justify-between gap-4">
        <AlertDescription>{description}</AlertDescription>
        {retry && (
          <Button
            variant="destructive"
            size="sm"
            aria-disabled={retrying}
            onClick={() => {
              if (!retrying) retry()
            }}
          >
            {retrying ? 'Retrying…' : 'Try Again'}
          </Button>
        )}
      </div>
    </Alert>
  )
}
