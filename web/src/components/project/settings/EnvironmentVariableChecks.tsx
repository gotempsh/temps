// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { CredentialProviderMark } from '@temps-sdk/ds'

import {
  CircleCheck,
  CircleHelp,
  Clock3,
  TriangleAlert,
  CircleX,
} from 'lucide-react'
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover'
import { Button } from '@/components/ui/button'
import { CHECK_STATUS_PRIORITY, type CheckStatus } from './http-checks'

/** Presentation model for HTTP check results; independent of credential providers. */
export interface EnvironmentVariableCheck {
  provider?: string | null
  id: string
  status: CheckStatus
  label: string
  detail: string
}

const appearance = {
  healthy: {
    icon: CircleCheck,
    color: 'text-emerald-700 dark:text-emerald-400',
  },
  warning: { icon: TriangleAlert, color: 'text-amber-700 dark:text-amber-400' },
  error: { icon: CircleX, color: 'text-red-700 dark:text-red-400' },
  pending: { icon: Clock3, color: 'text-muted-foreground' },
  unknown: { icon: CircleHelp, color: 'text-muted-foreground' },
}

export function EnvironmentVariableChecks({
  checks = [],
  onManage,
  compact = false,
}: {
  checks?: readonly EnvironmentVariableCheck[]
  onManage?: () => void
  /** Icon only, for dense tables; the label moves to the tooltip. */
  compact?: boolean
}) {
  const mostSevere = checks.reduce<EnvironmentVariableCheck | undefined>(
    (current, check) =>
      !current ||
      CHECK_STATUS_PRIORITY[check.status] >
        CHECK_STATUS_PRIORITY[current.status]
        ? check
        : current,
    undefined
  )
  const status = mostSevere?.status ?? 'healthy'
  const { icon: Icon, color } = appearance[status]
  const issues = checks.filter(
    (check) => check.status === 'warning' || check.status === 'error'
  )
  const label =
    issues.length > 1
      ? `${issues.length} issues`
      : status === 'healthy'
        ? 'No issues'
        : (mostSevere?.label ?? 'No issues')

  return (
    <Popover>
      <PopoverTrigger asChild>
        <button
          type="button"
          className={`inline-flex min-h-9 items-center gap-2 rounded-md text-sm whitespace-nowrap underline-offset-4 hover:underline focus-visible:outline-2 focus-visible:outline-ring focus-visible:outline-offset-4 ${color}`}
          aria-label={`Checks: ${label}`}
          title={compact ? label : undefined}
        >
          <Icon className="size-4 shrink-0" aria-hidden="true" />
          {mostSevere?.provider && (
            <CredentialProviderMark provider={mostSevere.provider} />
          )}
          {!compact && <span>{label}</span>}
        </button>
      </PopoverTrigger>
      <PopoverContent align="start" className="w-72 max-w-[calc(100vw-2rem)]">
        <p className="text-sm font-medium">Checks</p>
        {checks.length === 0 ? (
          <p className="mt-2 text-sm text-muted-foreground">
            No checks configured. Credential validity, expiration, and credits
            have not been verified.
          </p>
        ) : (
          <ul className="mt-3 space-y-3">
            {checks.map((check) => {
              const { icon: CheckIcon, color: checkColor } =
                appearance[check.status]
              return (
                <li key={check.id} className="flex items-start gap-2">
                  <CheckIcon
                    className={`mt-0.5 size-4 shrink-0 ${checkColor}`}
                    aria-hidden="true"
                  />
                  <div className="min-w-0">
                    <p className="flex items-center gap-2 text-sm font-medium">
                      <CredentialProviderMark provider={check.provider} />
                      {check.label}
                    </p>
                    <p className="mt-1 text-sm text-muted-foreground">
                      {check.detail}
                    </p>
                  </div>
                </li>
              )
            })}
          </ul>
        )}
        {onManage && (
          <Button
            type="button"
            variant="outline"
            size="sm"
            className="mt-3 w-full"
            onClick={onManage}
          >
            View details
          </Button>
        )}
      </PopoverContent>
    </Popover>
  )
}
