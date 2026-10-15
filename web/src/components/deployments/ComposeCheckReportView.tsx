// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ComposeCheckReport } from '@/api/client'
import { Badge } from '@/components/ui/badge'
import { CopyButton } from '@/components/ui/copy-button'
import {
  composeCommandLine,
  composeErrorClassHint,
  composeErrorClassLabel,
} from '@/lib/compose-check'
import { cn } from '@/lib/utils'
import { CheckCircle2, XCircle } from 'lucide-react'

interface ComposeCheckReportViewProps {
  report: ComposeCheckReport
  className?: string
}

/**
 * The deploy-time Compose check result: the failure and its remediation, the
 * exact `docker compose config` command with its exit code and sanitized
 * stderr, and the variables the file references. Variable values are never
 * part of a report.
 */
export function ComposeCheckReportView({
  report,
  className,
}: ComposeCheckReportViewProps) {
  const hint = composeErrorClassHint(report.error_class)
  const commandLine = report.command
    ? composeCommandLine(report.command.argv)
    : null
  return (
    <div className={cn('space-y-3 text-sm', className)}>
      <div
        className={cn(
          'flex items-start gap-2 rounded-md border px-3 py-2',
          report.passed
            ? 'border-emerald-500/30 bg-emerald-500/5 text-emerald-800 dark:text-emerald-300'
            : 'border-destructive/30 bg-destructive/5 text-destructive dark:text-red-300'
        )}
      >
        {report.passed ? (
          <CheckCircle2 className="mt-0.5 size-4 shrink-0" />
        ) : (
          <XCircle className="mt-0.5 size-4 shrink-0" />
        )}
        <div className="min-w-0 space-y-1">
          <p className="font-medium">
            {report.passed
              ? 'The Compose configuration passes the deploy-time checks'
              : composeErrorClassLabel(report.error_class)}
          </p>
          {report.error && (
            <p className="break-words text-foreground">{report.error}</p>
          )}
          {hint && !report.passed && (
            <p className="text-muted-foreground">{hint}</p>
          )}
        </div>
      </div>

      {report.resolution_reasons.length > 0 && (
        <p className="text-muted-foreground">
          <span className="font-medium text-foreground">
            docker compose config ran because of:
          </span>{' '}
          {report.resolution_reasons.join(', ')}
        </p>
      )}

      {report.command && commandLine && (
        <div className="space-y-1.5">
          <div className="flex items-center justify-between gap-2">
            <span className="font-medium">Command</span>
            <span className="text-xs text-muted-foreground">
              exit code {report.command.exit_code ?? 'none (killed)'}
            </span>
          </div>
          <div className="flex items-start gap-2 rounded-md border bg-muted/40 px-3 py-2">
            <code className="min-w-0 flex-1 break-all font-mono text-xs">
              {commandLine}
            </code>
            <CopyButton
              value={commandLine}
              label="Copy command"
              minimal
              className="shrink-0"
            />
          </div>
          {report.command.stderr && (
            <pre className="max-h-60 overflow-auto whitespace-pre-wrap break-words rounded-md border bg-muted/40 px-3 py-2 font-mono text-xs">
              {report.command.stderr}
            </pre>
          )}
        </div>
      )}

      {report.unset_variables.length > 0 && (
        <VariableList
          title="Unset variables (Compose substitutes an empty string)"
          names={report.unset_variables}
          tone="warning"
        />
      )}
      {report.referenced_variables.length > 0 && (
        <VariableList
          title="Referenced variables"
          names={report.referenced_variables}
        />
      )}

      {report.notes.length > 0 && (
        <ul className="list-disc space-y-1 pl-5 text-muted-foreground">
          {report.notes.map((note) => (
            <li key={note}>{note}</li>
          ))}
        </ul>
      )}
    </div>
  )
}

function VariableList({
  title,
  names,
  tone,
}: {
  title: string
  names: string[]
  tone?: 'warning'
}) {
  return (
    <div className="space-y-1.5">
      <span className="font-medium">{title}</span>
      <div className="flex flex-wrap gap-1.5">
        {names.map((name) => (
          <Badge
            key={name}
            variant="outline"
            className={cn(
              'font-mono font-normal',
              tone === 'warning' &&
                'border-amber-500/40 text-amber-800 dark:text-amber-300'
            )}
          >
            {name}
          </Badge>
        ))}
      </div>
    </div>
  )
}
