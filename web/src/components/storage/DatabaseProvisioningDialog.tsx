// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group'
import type { DatabaseProvisioningMode } from '@/api/client'
import {
  buildDatabaseProvisioningSelection,
  type DatabaseProvisioningSelection,
  isValidCustomDatabaseName,
} from '@/lib/database-provisioning'
import { cn } from '@/lib/utils'
import { Database, Layers3, Loader2, Network } from 'lucide-react'
import { useMemo, useState } from 'react'

export type { DatabaseProvisioningSelection }

const options: Array<{
  mode: DatabaseProvisioningMode
  title: string
  description: string
  icon: typeof Layers3
}> = [
  {
    mode: 'project_environment',
    title: 'Database per environment',
    description:
      'Production and previews stay isolated. This is the existing Temps behavior.',
    icon: Layers3,
  },
  {
    mode: 'project',
    title: 'Database per project',
    description:
      'Every environment in this project connects to the same database.',
    icon: Network,
  },
  {
    mode: 'custom',
    title: 'Custom database',
    description:
      'Use an exact database name, including one shared by multiple projects.',
    icon: Database,
  },
]

function databasePreview(
  mode: DatabaseProvisioningMode,
  projectSlug: string,
  customName: string
) {
  if (mode === 'custom') return customName || 'shared_database'
  if (mode === 'project') return projectSlug
  return `${projectSlug}_production`
}

export function DatabaseProvisioningDialog({
  open,
  onOpenChange,
  serviceName,
  projectSlug,
  isPending,
  onConfirm,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  serviceName: string
  projectSlug: string
  isPending: boolean
  onConfirm: (selection: DatabaseProvisioningSelection) => Promise<void>
}) {
  const [mode, setMode] = useState<DatabaseProvisioningMode>(
    'project_environment'
  )
  const [customName, setCustomName] = useState('')
  const [submitted, setSubmitted] = useState(false)

  const customNameIsValid = isValidCustomDatabaseName(customName)
  const customNameError =
    mode === 'custom' && submitted && !customNameIsValid
      ? 'Use lowercase letters, numbers, and underscores; start with a letter or underscore (63 characters max).'
      : null
  const preview = useMemo(
    () => databasePreview(mode, projectSlug, customName),
    [mode, projectSlug, customName]
  )

  const submit = async () => {
    setSubmitted(true)
    const selection = buildDatabaseProvisioningSelection(mode, customName)
    if (!selection) return
    await onConfirm(selection)
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-xl gap-5">
        <DialogHeader>
          <DialogTitle>Choose database isolation</DialogTitle>
          <DialogDescription>
            Decide which logical database deployments use when{' '}
            <span className="font-medium text-foreground">{serviceName}</span>{' '}
            is linked to{' '}
            <span className="font-medium text-foreground">{projectSlug}</span>.
          </DialogDescription>
        </DialogHeader>

        <RadioGroup
          value={mode}
          onValueChange={(value) => setMode(value as DatabaseProvisioningMode)}
          className="gap-2"
        >
          {options.map((option) => {
            const Icon = option.icon
            const selected = mode === option.mode
            return (
              <Label
                key={option.mode}
                htmlFor={`database-provisioning-${option.mode}`}
                className={cn(
                  'flex cursor-pointer items-start gap-3 rounded-lg border p-3.5 transition-colors',
                  'hover:border-foreground/20 hover:bg-muted/40',
                  selected && 'border-primary bg-primary/[0.04]'
                )}
              >
                <RadioGroupItem
                  id={`database-provisioning-${option.mode}`}
                  value={option.mode}
                  className="mt-0.5 shrink-0"
                />
                <span className="flex min-w-0 flex-1 gap-3">
                  <span
                    className={cn(
                      'flex size-8 shrink-0 items-center justify-center rounded-md bg-muted text-muted-foreground',
                      selected && 'bg-primary/10 text-primary'
                    )}
                  >
                    <Icon className="size-4" />
                  </span>
                  <span className="min-w-0">
                    <span className="flex items-center gap-2 text-sm font-medium text-foreground">
                      {option.title}
                      {option.mode === 'project_environment' ? (
                        <span className="rounded-full bg-muted px-2 py-0.5 text-[10px] font-medium uppercase tracking-wide text-muted-foreground">
                          Default
                        </span>
                      ) : null}
                    </span>
                    <span className="mt-0.5 block text-xs font-normal leading-relaxed text-muted-foreground">
                      {option.description}
                    </span>
                  </span>
                </span>
              </Label>
            )
          })}
        </RadioGroup>

        {mode === 'custom' ? (
          <div className="space-y-2 rounded-lg border bg-muted/20 p-3.5">
            <Label htmlFor="custom-database-name">Database name</Label>
            <Input
              id="custom-database-name"
              value={customName}
              onChange={(event) => setCustomName(event.target.value)}
              placeholder="shared_database"
              autoComplete="off"
              aria-invalid={Boolean(customNameError)}
              aria-describedby={
                customNameError ? 'custom-database-name-error' : undefined
              }
            />
            {customNameError ? (
              <p
                id="custom-database-name-error"
                className="text-xs leading-relaxed text-destructive"
              >
                {customNameError}
              </p>
            ) : (
              <p className="text-xs leading-relaxed text-muted-foreground">
                Temps reuses this database if it exists, or creates it on the
                first deployment.
              </p>
            )}
          </div>
        ) : null}

        <div className="flex items-center justify-between gap-4 rounded-md bg-muted/50 px-3 py-2.5 text-xs">
          <span className="text-muted-foreground">Production database</span>
          <code className="truncate rounded bg-background px-2 py-1 font-mono text-foreground shadow-sm ring-1 ring-border">
            {preview}
          </code>
        </div>

        <DialogFooter>
          <Button
            type="button"
            variant="outline"
            onClick={() => onOpenChange(false)}
            disabled={isPending}
          >
            Cancel
          </Button>
          <Button
            type="button"
            onClick={() => void submit()}
            disabled={isPending}
          >
            {isPending ? <Loader2 className="size-4 animate-spin" /> : null}
            Link database
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
