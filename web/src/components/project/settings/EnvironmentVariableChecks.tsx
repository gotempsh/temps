// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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

/** Presentation model for HTTP check results; independent of credential providers. */
export interface EnvironmentVariableCheck {
  id: string
  status: 'healthy' | 'warning' | 'error' | 'pending' | 'unknown'
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

const priority: Record<EnvironmentVariableCheck['status'], number> = {
  healthy: 0,
  pending: 1,
  unknown: 2,
  warning: 3,
  error: 4,
}

export function EnvironmentVariableChecks({
  checks = [],
  onManage,
}: {
  checks?: readonly EnvironmentVariableCheck[]
  onManage?: () => void
}) {
  const mostSevere = checks.reduce<EnvironmentVariableCheck | undefined>(
    (current, check) =>
      !current || priority[check.status] > priority[current.status]
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
        >
          <Icon className="size-4 shrink-0" aria-hidden="true" />
          <span>{label}</span>
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
                    <p className="text-sm font-medium">{check.label}</p>
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
