// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { SearchX } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'

export function TraceUnavailableState({
  traceId,
  onBack,
}: {
  traceId: string
  onBack: () => void
}) {
  return (
    <div>
      <Card>
        <CardContent className="flex flex-col items-center px-6 py-10 text-center">
          <SearchX className="mb-3 size-8 text-muted-foreground" />
          <p className="font-medium">Trace data is unavailable</p>
          <p className="mt-1 max-w-md text-sm text-muted-foreground">
            Its spans may have expired under your retention policy or may not
            have reached this project.
          </p>
          <code className="mt-4 max-w-full break-all rounded bg-muted px-3 py-1.5 text-xs">
            {traceId}
          </code>
          <Button size="sm" className="mt-4" onClick={onBack}>
            Back to traces
          </Button>
        </CardContent>
      </Card>
    </div>
  )
}
