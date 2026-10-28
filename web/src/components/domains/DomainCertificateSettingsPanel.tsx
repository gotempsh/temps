// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { certificateSettingsErrors } from './certificate-settings'
import type { DomainCertificateSettings } from '@/api/client'
import {
  getCertificateSettingsOptions,
  updateCertificateSettingsMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { ReadFailure } from '@/components/ui/read-failure'
import { Input } from '@temps-sdk/ui'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@temps-sdk/ui'
import { Field, Settings, SettingsGroup } from '@temps-sdk/ds'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { Link } from 'react-router'
import { toast } from 'sonner'

export function DomainCertificateSettingsPanel({
  domainId,
  wildcard,
}: {
  domainId: number
  wildcard: boolean
}) {
  const queryClient = useQueryClient()
  const options = getCertificateSettingsOptions({
    path: { domain_id: domainId },
  })
  const query = useQuery(options)
  const [draft, setDraft] = useState<DomainCertificateSettings | null>(null)
  const mutation = useMutation({
    ...updateCertificateSettingsMutation(),
    onMutate: async () => {
      await queryClient.cancelQueries({ queryKey: options.queryKey })
    },
    onSuccess: (saved) => {
      queryClient.setQueryData(options.queryKey, saved)
      setDraft(null)
      toast.success('Certificate settings saved')
    },
  })
  const settings = draft ?? query.data
  if (!settings) {
    return query.isError ? (
      <ReadFailure
        resource="Certificate settings"
        error={query.error}
        onRetry={() => void query.refetch()}
      />
    ) : (
      <p role="status">Loading certificate settings…</p>
    )
  }
  const update = (change: Partial<DomainCertificateSettings>) => {
    if (!mutation.isPending) {
      mutation.reset()
      setDraft({ ...settings, ...change })
    }
  }
  const errors = certificateSettingsErrors(settings, wildcard)
  return (
    <Settings
      embedded
      title="Certificate settings"
      description="Changes apply to new orders. Your current certificate keeps serving traffic, and any pending order keeps its original validation method."
      dirty={draft !== null}
      saving={mutation.isPending}
      errors={errors}
      saveLabel="Save certificate settings"
      onSubmit={(event) => {
        event.preventDefault()
        if (!mutation.isPending && !Object.values(errors).some(Boolean))
          mutation.mutate({ path: { domain_id: domainId }, body: settings })
      }}
    >
      {query.isError && (
        <ReadFailure
          resource="Certificate settings"
          error={query.error}
          onRetry={() => void query.refetch()}
        />
      )}
      {mutation.isError && (
        <p role="alert" className="text-sm text-destructive">
          Could not save certificate settings. Your edits are preserved.{' '}
          {typeof mutation.error === 'object' &&
          mutation.error &&
          'detail' in mutation.error
            ? String(mutation.error.detail)
            : 'Please try again.'}
        </p>
      )}
      <div className="max-w-5xl space-y-8">
        <SettingsGroup title="Validation & renewal" headingLevel="h3">
          <Field
            className="max-w-md"
            label="Validation method"
            description={
              wildcard
                ? 'Wildcard certificates require DNS-01.'
                : 'HTTP-01 requires this hostname to reach Temps on port 80. DNS-01 validates ownership using DNS TXT records.'
            }
            error={errors['Validation method']}
          >
            {(props) => (
              <Select
                value={settings.challenge_type}
                onValueChange={(challenge_type) => update({ challenge_type })}
              >
                <SelectTrigger {...props} className="max-w-md">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="http-01" disabled={wildcard}>
                    HTTP-01
                  </SelectItem>
                  <SelectItem value="dns-01">DNS-01</SelectItem>
                </SelectContent>
              </Select>
            )}
          </Field>
          <Field
            className="max-w-md"
            label="Renew before expiry (days)"
            description="1–90 days; checked daily. DNS-01 needs a configured DNS provider for automatic renewal."
            error={errors['Renewal window']}
          >
            {(props) => (
              <Input
                {...props}
                className="w-full"
                type="number"
                min={1}
                max={90}
                value={
                  Number.isFinite(settings.renewal_days)
                    ? settings.renewal_days
                    : ''
                }
                onChange={(event) =>
                  update({
                    renewal_days:
                      event.target.value === ''
                        ? NaN
                        : Number(event.target.value),
                  })
                }
              />
            )}
          </Field>
          {settings.challenge_type === 'dns-01' && (
            <p className="text-sm text-muted-foreground">
              <Link
                className="text-foreground underline underline-offset-4"
                to="/dns-providers"
              >
                Configure a DNS provider
              </Link>{' '}
              for automatic DNS-01 renewal.
            </p>
          )}
        </SettingsGroup>
        <SettingsGroup title="Expiry alerts" headingLevel="h3">
          <Field
            className="max-w-md"
            label="Certificate expiry alerts"
            description="Failed renewal attempts still raise failure alerts."
          >
            {(props) => (
              <Select
                value={
                  settings.alerts_enabled == null
                    ? 'inherit'
                    : String(settings.alerts_enabled)
                }
                onValueChange={(value) =>
                  update({
                    alerts_enabled:
                      value === 'inherit' ? null : value === 'true',
                  })
                }
              >
                <SelectTrigger {...props} className="max-w-md">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="inherit">
                    Use instance preference
                  </SelectItem>
                  <SelectItem value="true">Enabled</SelectItem>
                  <SelectItem value="false">Disabled</SelectItem>
                </SelectContent>
              </Select>
            )}
          </Field>
          <Field
            className="max-w-md"
            label="Alert before expiry (days)"
            optional
            description="Leave blank to inherit. Set this below the renewal window to avoid alerts before renewal is due."
            error={errors['Alert window']}
          >
            {(props) => (
              <Input
                {...props}
                className="w-full"
                type="number"
                min={1}
                max={90}
                value={settings.alert_days ?? ''}
                onChange={(event) =>
                  update({
                    alert_days:
                      event.target.value === ''
                        ? null
                        : Number(event.target.value),
                  })
                }
              />
            )}
          </Field>
          <Link className="text-sm underline" to="/monitoring/alerts">
            Manage instance alert preferences
          </Link>
        </SettingsGroup>
      </div>
    </Settings>
  )
}
