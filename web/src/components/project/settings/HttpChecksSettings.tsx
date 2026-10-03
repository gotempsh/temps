// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { CredentialProviderMark } from '@temps-sdk/ds'

import { CredentialProviderCatalog } from './CredentialProviderCatalog'
import { CheckLoading } from './CheckLoading'
import { useState, type ReactNode } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Link } from 'react-router'
import { CalendarClock, Globe } from 'lucide-react'
import {
  createHttpCheck,
  deleteHttpCheck,
  detectEnvCredential,
  detectSecretCredential,
  getHttpChecksCapabilities,
  listHttpCheckPresets,
  runHttpCheck,
  setHttpCheckEnabled,
  type HttpCheckSpec,
  type HttpCheckView,
  type ResponseField,
} from '@/api/client'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { EnvironmentVariableChecks } from './EnvironmentVariableChecks'
import { toast } from 'sonner'
import {
  DEFAULT_WARNING_DAYS,
  LOCAL_FORMATS,
  checkIndicators,
  checksFor,
  credentialSource,
  describeArtifact,
  httpChecksKey,
  markProvider,
  parseWarningDays,
  useHttpChecks,
  type CredentialSubject,
} from './http-checks'

function responseField(value: string): ResponseField {
  return value.startsWith('header:')
    ? { kind: 'header', value: value.slice(7) }
    : { kind: 'json_pointer', value }
}

const checkKinds = [
  {
    id: 'http',
    icon: Globe,
    title: 'Provider API',
    description:
      'Calls a read-only HTTPS endpoint to verify access, expiry, and credits.',
  },
  {
    id: 'local',
    icon: CalendarClock,
    title: 'Credential expiry',
    description:
      'Reads certificates, SSH certificates, OpenPGP keys, kubeconfigs and JWTs on this server and warns before they expire. Nothing is sent.',
  },
] as const

export function HttpChecksSettings({
  projectId,
  subject,
}: {
  projectId: number
  subject: CredentialSubject
}) {
  const queryClient = useQueryClient()
  const checks = useHttpChecks(projectId)
  const presets = useQuery({
    queryKey: ['http-check-presets', projectId],
    queryFn: async () =>
      (
        await listHttpCheckPresets({
          path: { project_id: projectId },
          throwOnError: true,
        })
      ).data,
  })
  const capabilities = useQuery({
    queryKey: ['http-check-capabilities', projectId],
    queryFn: async () =>
      (
        await getHttpChecksCapabilities({
          path: { project_id: projectId },
          throwOnError: true,
        })
      ).data,
  })
  const [kind, setKind] = useState<'http' | 'local'>('http')
  const [warningDays, setWarningDays] = useState(
    DEFAULT_WARNING_DAYS.join(', ')
  )
  const [presetId, setPresetId] = useState('custom')
  const [name, setName] = useState(`${subject.key} check`)
  const [url, setUrl] = useState('')
  const [header, setHeader] = useState('Authorization')
  const [prefix, setPrefix] = useState('Bearer ')
  const [expiry, setExpiry] = useState('')
  const [metric, setMetric] = useState('')
  const [warning, setWarning] = useState('20')
  const [critical, setCritical] = useState('5')
  const [comparison, setComparison] = useState<'below' | 'above'>('below')
  const [interval, setInterval] = useState('86400')
  const [deleteId, setDeleteId] = useState<number | null>(null)
  const chooseKind = (next: 'http' | 'local') => {
    setKind(next)
    setName(next === 'local' ? `${subject.key} expiry` : `${subject.key} check`)
  }
  const detection = useMutation({
    mutationFn: async () =>
      subject.kind === 'secret'
        ? (
            await detectSecretCredential({
              path: { project_id: projectId, secret_id: subject.id },
              throwOnError: true,
            })
          ).data
        : (
            await detectEnvCredential({
              path: { project_id: projectId, env_var_id: subject.id },
              throwOnError: true,
            })
          ).data,
    onError: () =>
      toast.error('Could not detect this credential. Check your permissions.'),
  })
  const invalidate = () =>
    queryClient.invalidateQueries({ queryKey: httpChecksKey(projectId) })
  const save = useMutation({
    mutationFn: async () => {
      if (kind === 'local') {
        const warning_days = parseWarningDays(warningDays)
        if (!warning_days)
          throw new Error(
            'Use 1–8 warning thresholds between 1 and 365 days, e.g. 30, 7, 1.'
          )
        return (
          await createHttpCheck({
            path: { project_id: projectId },
            body: {
              name: name.trim(),
              ...credentialSource(subject),
              kind: 'local',
              spec: null,
              local: { warning_days },
              interval_seconds: Number(interval),
              enabled: true,
            },
            throwOnError: true,
          })
        ).data
      }
      const selected = presets.data?.find((preset) => preset.id === presetId)
      const numeric_rules: HttpCheckSpec['numeric_rules'] = []
      if (metric.trim()) {
        if (
          !metric.startsWith('/') ||
          !warning.trim() ||
          !Number.isFinite(Number(warning)) ||
          (critical.trim() && !Number.isFinite(Number(critical)))
        )
          throw new Error('Use a JSON pointer and valid numeric thresholds.')
        numeric_rules.push({
          name: comparison === 'below' ? 'Balance' : 'Spending',
          field: responseField(metric.trim()),
          comparison,
          warning: Number(warning),
          critical: critical.trim() ? Number(critical) : null,
        })
      }
      const spec: HttpCheckSpec = {
        url: url.trim(),
        method: 'get',
        headers: selected?.spec.headers ?? {},
        credential_header: header.trim() || null,
        credential_prefix: prefix,
        accepted_statuses: [200],
        expiration: expiry.trim()
          ? { field: responseField(expiry.trim()), warning_days: [30, 7, 1] }
          : null,
        numeric_rules,
      }
      return (
        await createHttpCheck({
          path: { project_id: projectId },
          body: {
            name: name.trim(),
            ...credentialSource(subject),
            kind: 'http',
            spec,
            local: null,
            interval_seconds: Number(interval),
            enabled: true,
          },
          throwOnError: true,
        })
      ).data
    },
    onSuccess: () => {
      void invalidate()
      toast.success(
        kind === 'local'
          ? 'Expiry check added. The first run is scheduled.'
          : 'HTTP check added. The first run is scheduled.'
      )
    },
    onError: (error) =>
      toast.error(
        error instanceof Error && error.message.startsWith('Use ')
          ? error.message
          : 'Could not save the check. Check the endpoint, thresholds, credential, and your permissions.'
      ),
  })
  const action = useMutation({
    mutationFn: async ({
      check,
      operation,
    }: {
      check: HttpCheckView
      operation: 'run' | 'pause' | 'delete'
    }) => {
      const path = { project_id: projectId, check_id: check.id }
      if (operation === 'run') await runHttpCheck({ path, throwOnError: true })
      else if (operation === 'delete')
        await deleteHttpCheck({ path, throwOnError: true })
      else
        await setHttpCheckEnabled({
          path,
          body: { enabled: !check.enabled },
          throwOnError: true,
        })
    },
    onSuccess: () => {
      setDeleteId(null)
      void invalidate()
    },
    onError: () =>
      toast.error(
        'Could not update the check. It may already be running, or you may lack permission.'
      ),
  })
  const scopedChecks = checksFor(checks.data ?? [], subject)
  return (
    <section
      aria-label="Check configuration"
      className="w-full min-w-0 space-y-6"
    >
      <header className="space-y-2">
        <h2 className="text-2xl font-semibold break-all">{`Checks for ${subject.key}`}</h2>
        <p className="text-sm text-muted-foreground">
          Verify API access, expiration, and numeric thresholds with a read-only
          HTTPS request, or watch a certificate’s expiry without sending it
          anywhere. Write-only secrets can only use their recognized provider’s
          reviewed endpoint and authentication headers. Custom destinations
          require an explicit credential.
        </p>
      </header>
      {(checks.isPending || presets.isPending || capabilities.isPending) && (
        <CheckLoading label="Loading check configuration…" />
      )}
      {capabilities.data && !capabilities.data.alerts_configured && (
        <p className="text-sm text-muted-foreground">
          Checks run without alerts until you{' '}
          <Link className="underline" to={capabilities.data.alerts_setup_path}>
            configure notifications
          </Link>
          .
        </p>
      )}
      {checks.isError && (
        <p role="alert" className="text-sm text-destructive">
          Could not load checks.{' '}
          <Button variant="link" onClick={() => void checks.refetch()}>
            Retry
          </Button>
        </p>
      )}
      {scopedChecks.length > 0 && (
        <div className="divide-y">
          {scopedChecks.map((check) => (
            <div
              key={check.id}
              className="flex flex-col gap-2 py-3 sm:flex-row sm:items-center sm:justify-between"
            >
              <div className="min-w-0">
                <p className="flex items-center gap-2 font-medium text-sm break-all">
                  <CredentialProviderMark provider={markProvider(check)} />
                  {check.name}
                </p>
                <EnvironmentVariableChecks checks={checkIndicators([check])} />
              </div>
              <div className="flex flex-wrap gap-2">
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  disabled={action.isPending}
                  onClick={() => action.mutate({ check, operation: 'run' })}
                >
                  Check now
                </Button>
                <Button
                  type="button"
                  variant="ghost"
                  size="sm"
                  disabled={action.isPending}
                  onClick={() => action.mutate({ check, operation: 'pause' })}
                >
                  {check.enabled ? 'Pause' : 'Resume'}
                </Button>
                {deleteId === check.id ? (
                  <>
                    <Button
                      type="button"
                      variant="destructive"
                      size="sm"
                      disabled={action.isPending}
                      onClick={() =>
                        action.mutate({ check, operation: 'delete' })
                      }
                    >
                      Confirm delete
                    </Button>
                    <Button
                      type="button"
                      variant="ghost"
                      size="sm"
                      onClick={() => setDeleteId(null)}
                    >
                      Cancel
                    </Button>
                  </>
                ) : (
                  <Button
                    type="button"
                    variant="ghost"
                    size="sm"
                    onClick={() => setDeleteId(check.id)}
                  >
                    Delete
                  </Button>
                )}
              </div>
            </div>
          ))}
        </div>
      )}
      {presets.isError && (
        <p role="alert" className="text-sm text-destructive">
          Could not load provider catalog.{' '}
          <Button variant="link" onClick={() => void presets.refetch()}>
            Retry providers
          </Button>
        </p>
      )}
      <fieldset className="space-y-2">
        <legend className="text-sm font-medium">Check type</legend>
        <div className="grid grid-cols-1 gap-3 md:grid-cols-2">
          {checkKinds.map((option) => (
            <button
              key={option.id}
              type="button"
              aria-pressed={kind === option.id}
              onClick={() => chooseKind(option.id)}
              className={`flex items-start gap-3 rounded-lg border p-4 text-left transition-colors focus-visible:outline-2 focus-visible:outline-ring ${
                kind === option.id
                  ? 'border-primary bg-primary/5'
                  : 'hover:bg-muted/50'
              }`}
            >
              <option.icon
                className="mt-0.5 size-5 shrink-0 text-muted-foreground"
                aria-hidden="true"
              />
              <span>
                <span className="block text-sm font-medium">
                  {option.title}
                </span>
                <span className="mt-1 block text-sm text-muted-foreground">
                  {option.description}
                </span>
              </span>
            </button>
          ))}
        </div>
      </fieldset>
      {kind === 'http' && presets.data && (
        <CredentialProviderCatalog
          providers={presets.data ?? []}
          selectedId={presetId}
          onSelect={(id) => {
            setPresetId(id)
            const preset = presets.data?.find((p) => p.id === id)
            setUrl(preset?.spec.url ?? '')
            setHeader(preset?.spec.credential_header ?? 'Authorization')
            setPrefix(preset?.spec.credential_prefix ?? 'Bearer ')
            setName(
              preset ? `${preset.name} verification` : `${subject.key} check`
            )
            const field = preset?.spec.expiration?.field
            setExpiry(
              field
                ? field.kind === 'header'
                  ? `header:${field.value}`
                  : field.value
                : ''
            )
            setMetric('')
          }}
        />
      )}

      <form
        className="space-y-4 border-t pt-4"
        onSubmit={(event) => {
          event.preventDefault()
          save.mutate()
        }}
      >
        <div className="flex flex-wrap items-center justify-between gap-2">
          <h3 className="font-medium">Add a check</h3>
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => detection.mutate()}
            disabled={detection.isPending}
          >
            {detection.isPending ? 'Detecting…' : 'Detect provider'}
          </Button>
        </div>
        {detection.data && (
          <div className="space-y-2 text-sm text-muted-foreground">
            {detection.data.local_artifacts.length ? (
              <>
                <p>
                  This value holds items that expire. A local expiry check reads
                  them on this server.{' '}
                  {kind !== 'local' && (
                    <Button
                      type="button"
                      variant="link"
                      className="h-auto p-0"
                      onClick={() => chooseKind('local')}
                    >
                      Use a local expiry check
                    </Button>
                  )}
                </p>
                <ul className="list-disc space-y-1 pl-5">
                  {detection.data.local_artifacts.map((artifact) => (
                    <li key={`${artifact.label}-${artifact.expires_at}`}>
                      {describeArtifact(artifact)}
                    </li>
                  ))}
                </ul>
              </>
            ) : null}
            <p>
              {detection.data.candidates.length
                ? `Suggested matches: ${detection.data.candidates.map((candidate) => candidate.id).join(', ')}. Choose a template and verify its endpoint below.`
                : detection.data.local_artifacts.length
                  ? 'No API provider matched.'
                  : `No matching provider or expiring item found. Local expiry checks read ${LOCAL_FORMATS.join(', ')}; for anything else, configure a custom HTTP check below.`}{' '}
              Detection runs locally; no credential has been sent.
            </p>
          </div>
        )}
        <div className="grid gap-4 sm:grid-cols-2">
          <div className="space-y-2">
            <Label htmlFor="http-check-template">
              {kind === 'local' ? 'Type' : 'Template'}
            </Label>
            <p
              id="http-check-template"
              className="flex min-h-9 items-center gap-2 text-sm"
            >
              {kind === 'local' ? (
                'Credential expiry'
              ) : (
                <>
                  <CredentialProviderMark provider={presetId} />
                  {presets.data?.find((preset) => preset.id === presetId)
                    ?.name ?? 'Custom HTTP / Temps'}
                </>
              )}
            </p>
          </div>
          <div className="space-y-2">
            <Label htmlFor="http-check-name">Name</Label>
            <Input
              id="http-check-name"
              name="name"
              required
              maxLength={120}
              value={name}
              onChange={(e) => setName(e.target.value)}
            />
          </div>
        </div>
        {kind === 'local' ? (
          <LocalExpiryFields
            subject={subject}
            warningDays={warningDays}
            onWarningDaysChange={setWarningDays}
          />
        ) : (
          <HttpFields>
            {presetId !== 'custom' && (
              <p className="text-sm text-muted-foreground">
                {presets.data?.find((p) => p.id === presetId)?.description}
              </p>
            )}
            <div className="space-y-2">
              <Label htmlFor="http-check-url">Endpoint</Label>
              <Input
                id="http-check-url"
                name="url"
                type="url"
                required
                placeholder="https://api.example.com/account"
                value={url}
                onChange={(e) => setUrl(e.target.value)}
              />
              <p className="text-sm text-muted-foreground">
                Public HTTPS only. Requests use GET, expect HTTP 200, and never
                follow redirects.
              </p>
            </div>
            <div className="grid gap-4 sm:grid-cols-2">
              <div className="space-y-2">
                <Label htmlFor="http-check-header">Credential header</Label>
                <Input
                  id="http-check-header"
                  name="header"
                  placeholder="Authorization"
                  value={header}
                  onChange={(e) => setHeader(e.target.value)}
                />
              </div>
              <div className="space-y-2">
                <Label htmlFor="http-check-prefix">Header value prefix</Label>
                <Input
                  id="http-check-prefix"
                  name="prefix"
                  placeholder="Bearer (with a trailing space)"
                  value={prefix}
                  onChange={(e) => setPrefix(e.target.value)}
                />
              </div>
            </div>
            <p className="text-sm text-muted-foreground">
              Uses the stored value of {subject.key}. Saving this check
              authorizes sending it to the endpoint above.
            </p>
            <div className="space-y-2">
              <Label htmlFor="http-check-expiry">
                Expiration field (optional)
              </Label>
              <Input
                id="http-check-expiry"
                name="expiry"
                placeholder="/expires_at or header:github-authentication-token-expiration"
                value={expiry}
                onChange={(e) => setExpiry(e.target.value)}
              />
              <p className="text-sm text-muted-foreground">
                Warns at 30, 7, and 1 day. Missing expiration data is shown as
                unknown.
              </p>
            </div>
            <div className="space-y-2">
              <Label htmlFor="http-check-metric">
                Balance or spending field (optional)
              </Label>
              <Input
                id="http-check-metric"
                name="metric"
                placeholder="/balance"
                value={metric}
                onChange={(e) => setMetric(e.target.value)}
              />
            </div>
            {metric && (
              <div className="grid gap-4 sm:grid-cols-3">
                <div className="space-y-2">
                  <Label htmlFor="http-check-comparison">Alert when</Label>
                  <Select
                    value={comparison}
                    onValueChange={(value) =>
                      setComparison(value as 'above' | 'below')
                    }
                  >
                    <SelectTrigger id="http-check-comparison">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value="below">Below threshold</SelectItem>
                      <SelectItem value="above">Above threshold</SelectItem>
                    </SelectContent>
                  </Select>
                </div>
                <div className="space-y-2">
                  <Label htmlFor="http-check-warning">Warning</Label>
                  <Input
                    id="http-check-warning"
                    name="warning"
                    type="number"
                    step="any"
                    required
                    value={warning}
                    onChange={(e) => setWarning(e.target.value)}
                  />
                </div>
                <div className="space-y-2">
                  <Label htmlFor="http-check-critical">
                    Critical (optional)
                  </Label>
                  <Input
                    id="http-check-critical"
                    name="critical"
                    type="number"
                    step="any"
                    value={critical}
                    onChange={(e) => setCritical(e.target.value)}
                  />
                </div>
              </div>
            )}
          </HttpFields>
        )}
        <div className="flex flex-col gap-3 sm:flex-row sm:items-end sm:justify-between">
          <div className="space-y-2">
            <Label htmlFor="http-check-interval">Schedule</Label>
            <Select value={interval} onValueChange={setInterval}>
              <SelectTrigger
                id="http-check-interval"
                className="w-full sm:w-48"
              >
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="86400">Daily</SelectItem>
                <SelectItem value="21600">Every 6 hours</SelectItem>
                <SelectItem value="3600">Hourly</SelectItem>
              </SelectContent>
            </Select>
          </div>
          <Button type="submit" disabled={save.isPending}>
            {save.isPending ? 'Saving…' : 'Save check'}
          </Button>
        </div>
      </form>
    </section>
  )
}

/** Groups the HTTP-only fields so the certificate form can replace them. */
function HttpFields({ children }: { children: ReactNode }) {
  return <div className="space-y-4">{children}</div>
}

function LocalExpiryFields({
  subject,
  warningDays,
  onWarningDaysChange,
}: {
  subject: CredentialSubject
  warningDays: string
  onWarningDaysChange: (value: string) => void
}) {
  const valid = parseWarningDays(warningDays) !== null
  return (
    <div className="space-y-4">
      <div className="space-y-2">
        <Label htmlFor="expiry-warning-days">Warn before expiry (days)</Label>
        <Input
          id="expiry-warning-days"
          name="warning_days"
          inputMode="numeric"
          required
          aria-invalid={!valid}
          aria-describedby="expiry-warning-days-help"
          className="w-full sm:w-48"
          value={warningDays}
          onChange={(e) => onWarningDaysChange(e.target.value)}
        />
        <p
          id="expiry-warning-days-help"
          className={`text-sm ${valid ? 'text-muted-foreground' : 'text-destructive'}`}
        >
          Up to 8 thresholds between 1 and 365 days, e.g. 30, 7, 1. Every
          expiring item in the value is checked; the earliest decides the
          result.
        </p>
      </div>
      <p className="text-sm text-muted-foreground">
        Reads the stored value of {subject.key} on this server, raw or
        base64-encoded: {LOCAL_FORMATS.join(', ')}. The value is never sent
        anywhere, private key material is never interpreted, and kubeconfig file
        paths and exec plugins are ignored.
      </p>
    </div>
  )
}
