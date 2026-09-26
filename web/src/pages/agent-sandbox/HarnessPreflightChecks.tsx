// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useId, useState } from 'react'
import { useMutation } from '@tanstack/react-query'
import {
  AlertTriangle,
  CheckCircle2,
  ChevronRight,
  CircleMinus,
  XCircle,
} from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { Label } from '@/components/ui/label'
import { CopyButton } from '@/components/ui/copy-button'
import {
  runAiProviderPreflight,
  runAiProviderSmoke,
  type HarnessCheckReport,
} from '@/api/client'
import { problemDetail } from '@/lib/api-problem'
import { HarnessCheckProgress } from './HarnessCheckProgress'

const statuses = {
  passed: { label: 'Passed', Icon: CheckCircle2, color: 'text-primary' },
  warning: {
    label: 'Warning',
    Icon: AlertTriangle,
    color: 'text-muted-foreground',
  },
  failed: { label: 'Failed', Icon: XCircle, color: 'text-destructive' },
  not_tested: {
    label: 'Not tested',
    Icon: CircleMinus,
    color: 'text-muted-foreground',
  },
} as const

export function HarnessCheckResults({
  report,
}: {
  report: HarnessCheckReport
}) {
  return (
    <div className="space-y-3">
      <p className="text-sm font-medium" role="status">
        {report.mode === 'smoke' ? 'Smoke test' : 'Setup check'}:{' '}
        {statuses[report.overall].label}
      </p>
      <ul className="divide-y">
        {report.checks.map((check) => {
          const { Icon, label, color } = statuses[check.status]
          return (
            <li key={check.id} className="py-2">
              <details className="group">
                <summary className="flex cursor-pointer flex-wrap items-center gap-2 rounded-sm text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring">
                  <Icon
                    aria-hidden="true"
                    className={`size-4 shrink-0 ${color}`}
                  />
                  <span className="flex-1">{check.label}</span>
                  <span className={`text-xs ${color}`}>{label}</span>
                  <span className="font-mono text-xs tabular-nums text-muted-foreground">
                    {check.status === 'not_tested'
                      ? '—'
                      : `${(check.duration_ms / 1000).toFixed(1)}s`}
                  </span>
                  <ChevronRight
                    aria-hidden="true"
                    className="size-3 shrink-0 group-open:rotate-90"
                  />
                </summary>
                <div className="mt-2 space-y-1 pl-6 text-xs text-muted-foreground break-words">
                  <p>{check.detail}</p>
                  {check.action && <p>{check.action}</p>}
                </div>
              </details>
              {check.status === 'failed' && (
                <p className="mt-1 pl-6 text-xs text-destructive break-words">
                  {check.action ?? check.detail}
                </p>
              )}
            </li>
          )
        })}
      </ul>
      <p className="text-xs text-muted-foreground break-all">
        Checked {new Date(report.checked_at).toLocaleString()} · Diagnostic ID:{' '}
        <code>{report.diagnostic_id}</code>
        <CopyButton
          value={report.diagnostic_id}
          label="Copy diagnostic ID"
          minimal
          className="ml-1 align-middle"
        />
      </p>
    </div>
  )
}

/** Mount with a configuration key so saved credential/model changes discard stale reports. */
export function HarnessPreflightChecks({
  providerId,
  model,
  credentialSaved,
  disabled = false,
}: {
  providerId: string
  model?: string | null
  credentialSaved: boolean
  disabled?: boolean
}) {
  const consentId = useId()
  const [consent, setConsent] = useState(false)
  const mutation = useMutation({
    // Retrying a paid model request must remain an explicit user action.
    retry: false,
    mutationFn: async (mode: 'preflight' | 'smoke') => {
      const response =
        mode === 'preflight'
          ? await runAiProviderPreflight({
              path: { provider_id: providerId },
              throwOnError: true,
            })
          : await runAiProviderSmoke({
              path: { provider_id: providerId },
              body: { consent, model: model || undefined },
              throwOnError: true,
            })
      return response.data
    },
  })
  return (
    <section
      aria-label="Harness diagnostics"
      className="space-y-3 border-t pt-4"
    >
      <div>
        <h3 className="text-sm font-medium">Check your setup</h3>
        <p className="mt-1 text-xs text-muted-foreground">
          Checks a temporary managed sandbox and may download required images.
          No model request is sent during the setup check.
        </p>
      </div>
      <div className="flex flex-wrap gap-2">
        <Button
          variant="outline"
          size="sm"
          disabled={mutation.isPending || disabled}
          onClick={() => mutation.mutate('preflight')}
        >
          Check setup
        </Button>
        <Button
          variant="outline"
          size="sm"
          disabled={
            mutation.isPending || disabled || !credentialSaved || !consent
          }
          onClick={() => mutation.mutate('smoke')}
        >
          Run smoke test
        </Button>
      </div>
      <div className="flex items-start gap-2">
        <Checkbox
          id={consentId}
          checked={consent}
          disabled={mutation.isPending || disabled || !credentialSaved}
          onCheckedChange={(checked) => setConsent(checked === true)}
        />
        <Label htmlFor={consentId} className="text-xs font-normal leading-4">
          Allow a small model request using my saved credential and provider
          allowance.
        </Label>
      </div>
      {!credentialSaved && (
        <p className="text-xs text-muted-foreground">
          Save a credential to run the smoke test. You can check infrastructure
          first.
        </p>
      )}
      {credentialSaved && (
        <p className="text-xs text-muted-foreground break-words">
          Smoke test model:{' '}
          <code>{model || 'Harness verification default'}</code>. This tests one
          reply, not access to every model.
        </p>
      )}
      {mutation.isPending && (
        <HarnessCheckProgress
          label={
            mutation.variables === 'smoke'
              ? 'Testing a model reply…'
              : 'Checking setup…'
          }
        />
      )}
      {mutation.isError && (
        <p role="alert" className="text-sm text-destructive">
          {problemDetail(
            mutation.error,
            'The check could not finish. Retry or ask your administrator to check the server logs.'
          )}
        </p>
      )}
      {!mutation.isPending && !mutation.isError && mutation.data && (
        <HarnessCheckResults report={mutation.data} />
      )}
      <p className="text-xs text-muted-foreground">
        Results describe this check, not continuous availability. The smoke test
        does not change your saved credential. This does not verify a reply in
        your persistent workspace.
      </p>
    </section>
  )
}
