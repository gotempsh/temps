// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Wand2 } from 'lucide-react'
import { Link } from 'react-router'

/**
 * Shown on a run of the earlier, CLI-driven autofixer (ADR-115 D2): the run
 * stays readable, and fixing the error again starts an autofix incident from
 * its error group.
 */
export function LegacyAutofixNotice({
  errorGroupId,
}: {
  errorGroupId: number | null | undefined
}) {
  return (
    <Alert>
      <Wand2 className="h-4 w-4" />
      <AlertTitle>Autofix now runs as an incident</AlertTitle>
      <AlertDescription className="space-y-2">
        <p>
          This run used the earlier autofixer. To fix this error again, open the
          error group and start an autofix incident; it runs on your AI gateway.
        </p>
        {errorGroupId != null && (
          <Button size="sm" variant="outline" asChild>
            <Link to={`../errors/${errorGroupId}`}>Open the error group</Link>
          </Button>
        )}
      </AlertDescription>
    </Alert>
  )
}
