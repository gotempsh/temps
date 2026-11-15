// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { runDeploymentComposeCheckMutation } from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { useMutation } from '@tanstack/react-query'
import { FileCheck2, Loader2 } from 'lucide-react'
import { useState } from 'react'
import { ComposeCheckReportView } from './ComposeCheckReportView'

interface ComposeCheckActionProps {
  projectId: number
  deploymentId: number
}

/**
 * "Run compose check": runs the deploy job's Compose resolution and policy
 * validation for this deployment's revision with the project's current
 * override, exclusions, security policy and environment variables, without
 * deploying. Requires write access to the project.
 */
export function ComposeCheckAction({
  projectId,
  deploymentId,
}: ComposeCheckActionProps) {
  const [open, setOpen] = useState(false)
  const check = useMutation({
    ...runDeploymentComposeCheckMutation(),
    meta: { errorTitle: 'Failed to run the Compose check' },
  })

  const run = () => {
    check.mutate(
      { path: { project_id: projectId, deployment_id: deploymentId } },
      { onSuccess: () => setOpen(true) }
    )
  }

  return (
    <div className="flex flex-col gap-2 rounded-md border px-3 py-2 sm:flex-row sm:items-center sm:justify-between">
      <div className="min-w-0 text-sm">
        <p className="font-medium">Compose configuration</p>
        <p className="text-muted-foreground">
          Check this revision against the current override, security policy and
          environment variables without deploying. Shows the exact{' '}
          <code className="font-mono text-xs">docker compose config</code>{' '}
          command and its output.
        </p>
      </div>
      <Button
        variant="outline"
        size="sm"
        className="shrink-0"
        onClick={run}
        disabled={check.isPending}
      >
        {check.isPending ? (
          <Loader2 className="size-4 animate-spin" />
        ) : (
          <FileCheck2 className="size-4" />
        )}
        Run compose check
      </Button>
      <Dialog open={open && !!check.data} onOpenChange={setOpen}>
        <DialogContent className="max-h-[85vh] overflow-y-auto sm:max-w-2xl">
          <DialogHeader>
            <DialogTitle>Compose check</DialogTitle>
            <DialogDescription>
              Deployment #{deploymentId}. Variable values are never shown.
            </DialogDescription>
          </DialogHeader>
          {check.data && <ComposeCheckReportView report={check.data} />}
        </DialogContent>
      </Dialog>
    </div>
  )
}
