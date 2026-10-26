// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// *Make it reachable* (ADR 064 D1, D4): the server plans the ways in that
// fit this cluster, the user picks one and answers what it still needs,
// and the server carries it out as a run that can be resumed and undone.
// Apply names the stored plan; when the cluster changed since, nothing is
// created and the dialog shows the new plan instead.

import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  AlertTriangle,
  CheckCircle2,
  Circle,
  CircleDot,
  ExternalLink,
  Loader2,
  MinusCircle,
  XCircle,
} from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { useForm, useWatch } from 'react-hook-form'
import { Link } from 'react-router'
import { toast } from 'sonner'
import { z } from 'zod'
import {
  planEnvironmentReachability,
  type Decision,
  type PlanOption,
  type ReachabilityPlan,
  type RunStep,
  type RunView,
  type SetupField,
} from '@/api/client'
import {
  applyEnvironmentReachabilityMutation,
  getEnvironmentReachabilityQueryKey,
  getReachabilityRunOptions,
  listDnsProvidersOptions,
  listReachabilityRunsOptions,
  decideReachabilityConflictMutation,
  resumeReachabilityRunMutation,
  undoReachabilityRunMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { CloudflareTokenCheck } from '@/components/domains/CloudflareTokenGuide'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { CopyButton } from '@/components/ui/copy-button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import {
  Form,
  FormControl,
  FormDescription,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { useDebounce } from '@/hooks/useDebounce'
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'
import { problemDetail } from '@/lib/api-problem'
import {
  addressUrl,
  applyValues,
  canResumeRun,
  canUndoRun,
  conflictChoices,
  conflictSubject,
  conflictsToDecide,
  defaultOption,
  isActiveRun,
  missingAnswers,
  planIsCurrent,
  privateDnsWarning,
  resumeNeedsToken,
  stalePlan,
} from '@/lib/reachability'
import { cn } from '@/lib/utils'

const schema = z.object({
  /** Who can open the address (ADR 064 D5). */
  audience: z.enum(['public', 'private']),
  address: z.string().trim(),
  option: z.string().min(1, 'Pick a way in.'),
  values: z.record(z.string(), z.string()),
  /** The user accepted the option's `confirmation`. */
  confirmed: z.boolean(),
})
type Values = z.infer<typeof schema>

export interface MakeReachableDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  projectId: number
  environmentId: number
  environmentName?: string
}

export function MakeReachableDialog({
  open,
  onOpenChange,
  projectId,
  environmentId,
  environmentName,
}: MakeReachableDialogProps) {
  const queryClient = useQueryClient()
  const path = { project_id: projectId, environment_id: environmentId }
  const [runId, setRunId] = useState<number | null>(null)
  const { handleSensitiveActionError, verificationDialog } =
    useSensitiveActionVerification()

  const form = useForm<Values>({
    resolver: zodResolver(schema),
    defaultValues: {
      audience: 'public',
      address: '',
      option: '',
      values: {},
      confirmed: false,
    },
  })
  const typedAddress = useWatch({ control: form.control, name: 'address' })
  const requested = useDebounce(typedAddress.trim(), 500)
  const chosen = useWatch({ control: form.control, name: 'option' })
  const answers = useWatch({ control: form.control, name: 'values' })
  const audience = useWatch({ control: form.control, name: 'audience' })
  const planKey = [
    'reachabilityPlan',
    projectId,
    environmentId,
    audience,
    requested,
  ]

  // A run already going on this environment is picked up, not duplicated.
  const runs = useQuery({
    ...listReachabilityRunsOptions({ path }),
    enabled: open && runId === null,
    staleTime: 0,
  })
  const latest = runs.data?.[0]
  // Adopted once, during render (not in an effect), so "Start over" does not
  // snap back to the run it just left.
  const [adopted, setAdopted] = useState(false)
  if (
    !adopted &&
    runId === null &&
    latest &&
    (isActiveRun(latest.state) ||
      latest.state === 'waiting' ||
      latest.state === 'interrupted')
  ) {
    setAdopted(true)
    setRunId(latest.id)
  }

  const plan = useQuery({
    queryKey: planKey,
    queryFn: async ({ signal }) => {
      const { data } = await planEnvironmentReachability({
        path,
        body: { audience, address: requested || null },
        signal,
        throwOnError: true,
      })
      return data
    },
    enabled: open && runId === null,
    placeholderData: (previous) => previous,
    retry: false,
  })

  // Until the plan for the address typed now arrives, nothing can be applied.
  const planCurrent = planIsCurrent({
    typed: typedAddress,
    requested,
    placeholder: plan.isPlaceholderData,
  })

  // The suggested address fills the field once; the chosen option follows
  // the plan's recommendation unless the user picked one that still fits.
  const suggested = plan.data?.address
  useEffect(() => {
    if (suggested && !form.getValues('address'))
      form.setValue('address', suggested)
  }, [suggested, form])
  const options = useMemo(() => plan.data?.options ?? [], [plan.data])
  useEffect(() => {
    if (!options.some((o) => o.kind === form.getValues('option'))) {
      form.setValue('option', defaultOption(options)?.kind ?? '')
      form.setValue('confirmed', false)
    }
  }, [options, form])
  const option = options.find((o) => o.kind === chosen)
  const fields = (option?.fields ?? []).filter((f) => f.key !== 'address')

  const run = useQuery({
    ...getReachabilityRunOptions({ path: { ...path, run_id: runId ?? 0 } }),
    enabled: open && runId !== null,
    refetchInterval: (query) => {
      const state = query.state.data?.state
      if (!state || isActiveRun(state)) return 2_000
      return state === 'waiting' ? 15_000 : false
    },
  })
  const runState = run.data?.state
  useEffect(() => {
    if (runState && !isActiveRun(runState))
      void queryClient.invalidateQueries({
        queryKey: getEnvironmentReachabilityQueryKey({ path }),
      })
    // `path` is rebuilt every render; its ids are the dependency.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [runState, queryClient, projectId, environmentId])

  const apply = useMutation({
    ...applyEnvironmentReachabilityMutation(),
    onSuccess: (started) => setRunId(started.id),
    onError: (error, variables) => {
      // Changing who an existing address is for needs a recent
      // confirmation (ADR 064 Security).
      if (handleSensitiveActionError(error, () => apply.mutate(variables)))
        return
      // Nothing was created: show the plan the cluster has now.
      const fresh = stalePlan(error)
      if (fresh) {
        queryClient.setQueryData(planKey, fresh)
        form.setValue('confirmed', false)
      }
    },
  })
  const showRun = (view: RunView) =>
    queryClient.setQueryData(
      getReachabilityRunOptions({ path: { ...path, run_id: view.id } })
        .queryKey,
      view
    )
  const resumeRun = useMutation({
    ...resumeReachabilityRunMutation(),
    onSuccess: showRun,
  })
  const undo = useMutation({
    ...undoReachabilityRunMutation(),
    onSuccess: (undone) => {
      showRun(undone)
      if (undone.state === 'undone')
        toast.success('Undone: everything it created is removed')
    },
    onError: (error, variables) => {
      handleSensitiveActionError(error, () => undo.mutate(variables))
    },
  })

  const decide = useMutation({
    ...decideReachabilityConflictMutation(),
    onSuccess: showRun,
    onError: (error, variables) => {
      handleSensitiveActionError(error, () => decide.mutate(variables))
    },
  })

  const submit = (values: Values) => {
    if (!option) return
    const missing = missingAnswers(fields, values.values)
    if (missing.length > 0) {
      form.setError('values', {
        message: `Still needed: ${missing.join(', ')}.`,
      })
      return
    }
    const planId = planCurrent ? plan.data?.plan_id : undefined
    if (!planId) {
      form.setError('values', {
        message: 'The plan is still being worked out. Try again in a moment.',
      })
      return
    }
    if (option.confirmation && !values.confirmed) {
      form.setError('confirmed', {
        message: 'Confirm this change to continue.',
      })
      return
    }
    apply.mutate({
      path,
      body: {
        plan_id: planId,
        option: option.kind,
        answers: applyValues(
          fields,
          values.values,
          values.address.trim() || plan.data?.address
        ),
        confirmed: Boolean(option.confirmation) && values.confirmed,
      },
    })
  }

  const close = (next: boolean) => {
    if (!next) {
      void queryClient.invalidateQueries({
        queryKey: getEnvironmentReachabilityQueryKey({ path }),
      })
    }
    onOpenChange(next)
  }

  return (
    <>
      <Dialog open={open} onOpenChange={close}>
        <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-2xl">
          <DialogHeader>
            <DialogTitle>Make it reachable</DialogTitle>
            <DialogDescription>
              Give {environmentName ?? 'this environment'} an address, open to
              anyone on the internet or only to your private network. Temps sets
              up only what is missing, and can undo it.
            </DialogDescription>
          </DialogHeader>
          {runId !== null ? (
            <RunProgress
              run={run.data}
              loadError={
                run.isError
                  ? problemDetail(
                      run.error,
                      'Could not load the run. Close this and open it again.'
                    )
                  : null
              }
              undoing={undo.isPending}
              undoError={
                undo.isError
                  ? problemDetail(
                      undo.error,
                      'Undo failed. Try again; nothing else was changed.'
                    )
                  : null
              }
              onUndo={() =>
                run.data &&
                undo.mutate({ path: { ...path, run_id: run.data.id } })
              }
              resuming={resumeRun.isPending}
              resumeError={
                resumeRun.isError
                  ? problemDetail(
                      resumeRun.error,
                      'Could not resume. Try again; nothing else was changed.'
                    )
                  : null
              }
              onResume={(token) =>
                run.data &&
                resumeRun.mutate({
                  path: { ...path, run_id: run.data.id },
                  body: {
                    answers: token ? { cloudflare_api_token: token } : {},
                  },
                })
              }
              deciding={decide.isPending}
              decideError={
                decide.isError
                  ? problemDetail(
                      decide.error,
                      'Could not save your decision. Try again; nothing was changed.'
                    )
                  : null
              }
              onDecide={(key, decision) =>
                run.data &&
                decide.mutate({
                  path: { ...path, run_id: run.data.id },
                  body: { key, decision },
                })
              }
              onStartOver={() => {
                setRunId(null)
                apply.reset()
                undo.reset()
                resumeRun.reset()
                decide.reset()
              }}
            />
          ) : (
            <Form {...form}>
              <form
                id="make-reachable"
                onSubmit={form.handleSubmit(submit)}
                className="space-y-5"
              >
                <FormField
                  control={form.control}
                  name="audience"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Who can open it</FormLabel>
                      <FormControl>
                        <RadioGroup
                          aria-label="Who can open it"
                          value={field.value}
                          onValueChange={(next) => {
                            field.onChange(next)
                            form.setValue('option', '')
                            form.setValue('confirmed', false)
                          }}
                          className="grid gap-2 sm:grid-cols-2"
                        >
                          {AUDIENCES.map((choice) => (
                            <label
                              key={choice.value}
                              className={cn(
                                'flex cursor-pointer items-start gap-2 rounded-md border p-3',
                                field.value === choice.value
                                  ? 'border-primary'
                                  : 'border-border hover:border-primary/50'
                              )}
                            >
                              <RadioGroupItem
                                value={choice.value}
                                className="mt-0.5"
                              />
                              <span className="space-y-0.5">
                                <span className="block text-sm font-medium">
                                  {choice.label}
                                </span>
                                <span className="block text-xs text-muted-foreground">
                                  {choice.help}
                                </span>
                              </span>
                            </label>
                          ))}
                        </RadioGroup>
                      </FormControl>
                    </FormItem>
                  )}
                />
                <FormField
                  control={form.control}
                  name="address"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Address</FormLabel>
                      <FormControl>
                        <Input
                          autoComplete="off"
                          placeholder="myapp.example.com"
                          {...field}
                        />
                      </FormControl>
                      <FormDescription>
                        The hostname people will open, on a domain you own.
                      </FormDescription>
                      {plan.data?.address_error && (
                        <p className="text-sm text-destructive">
                          {plan.data.address_error}
                        </p>
                      )}
                    </FormItem>
                  )}
                />
                <PlanChoices
                  plan={plan.data}
                  loading={plan.isPending || !planCurrent}
                  error={
                    plan.isError
                      ? problemDetail(
                          plan.error,
                          'Could not work out a plan. Check the address and try again.'
                        )
                      : null
                  }
                  value={chosen}
                  onChange={(kind) => {
                    form.setValue('option', kind)
                    form.setValue('confirmed', false)
                  }}
                />
                {option && fields.length > 0 && (
                  <div className="space-y-4">
                    {fields.map((setupField) => (
                      <SetupFieldInput
                        key={setupField.key}
                        field={setupField}
                        control={form.control}
                        zone={typedAddress}
                        token={answers[setupField.key] ?? ''}
                      />
                    ))}
                  </div>
                )}
                {option?.confirmation && (
                  <FormField
                    control={form.control}
                    name="confirmed"
                    render={({ field }) => (
                      <FormItem className="flex items-start gap-3 space-y-0 rounded-md border p-3">
                        <FormControl>
                          <Checkbox
                            checked={field.value}
                            onCheckedChange={(checked) =>
                              field.onChange(checked === true)
                            }
                          />
                        </FormControl>
                        <div className="space-y-1">
                          <FormLabel className="font-normal">
                            {option.confirmation}
                          </FormLabel>
                          <FormDescription>
                            Undo puts the earlier choice back.
                          </FormDescription>
                          <FormMessage />
                        </div>
                      </FormItem>
                    )}
                  />
                )}
                <FormField
                  control={form.control}
                  name="values"
                  render={() => (
                    <FormItem>
                      <FormMessage />
                    </FormItem>
                  )}
                />
                {option && privateDnsWarning(option) && (
                  <Alert variant="warning">
                    <AlertTriangle className="h-4 w-4" />
                    <AlertDescription>
                      {privateDnsWarning(option)}
                    </AlertDescription>
                  </Alert>
                )}
                {apply.isError && (
                  <Alert
                    variant={stalePlan(apply.error) ? 'warning' : 'destructive'}
                  >
                    <AlertDescription>
                      {problemDetail(
                        apply.error,
                        'Could not start. Check the answers above and try again.'
                      )}
                    </AlertDescription>
                  </Alert>
                )}
              </form>
            </Form>
          )}
          <DialogFooter>
            {runId === null ? (
              <>
                <Button variant="outline" onClick={() => close(false)}>
                  Cancel
                </Button>
                <Button
                  type="submit"
                  form="make-reachable"
                  disabled={!option || !planCurrent || apply.isPending}
                >
                  {option && !planCurrent && (
                    <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  )}
                  {option ? option.title : 'Make it reachable'}
                </Button>
              </>
            ) : (
              <Button variant="outline" onClick={() => close(false)}>
                Close
              </Button>
            )}
          </DialogFooter>
        </DialogContent>
      </Dialog>
      {verificationDialog}
    </>
  )
}

const AUDIENCES = [
  {
    value: 'public',
    label: 'Anyone on the internet',
    help: 'A public address with HTTPS.',
  },
  {
    value: 'private',
    label: 'Only my private network',
    help: 'Served by a worker on your tailnet or LAN; nobody else can connect.',
  },
] as const

function PlanChoices({
  plan,
  loading,
  error,
  value,
  onChange,
}: {
  plan: ReachabilityPlan | undefined
  loading: boolean
  error: string | null
  value: string
  onChange: (kind: string) => void
}) {
  if (loading && !plan)
    return (
      <div className="space-y-2">
        <Skeleton className="h-16 w-full" />
        <Skeleton className="h-16 w-full" />
      </div>
    )
  if (error)
    return (
      <Alert variant="destructive">
        <AlertDescription>{error}</AlertDescription>
      </Alert>
    )
  if (!plan) return null
  if (plan.options.length === 0)
    return (
      <Alert variant="warning">
        <AlertTriangle className="h-4 w-4" />
        <AlertDescription className="space-y-2">
          <p>{plan.blocked?.reason ?? 'No way in fits this address yet.'}</p>
          {plan.blocked?.action?.command && (
            <div className="flex items-center gap-2">
              <code className="rounded bg-muted px-1.5 py-0.5 text-xs">
                {plan.blocked.action.command}
              </code>
              <CopyButton value={plan.blocked.action.command} minimal />
            </div>
          )}
          {plan.blocked?.action?.href && (
            <Link className="underline" to={plan.blocked.action.href}>
              {plan.blocked.action.label}
            </Link>
          )}
        </AlertDescription>
      </Alert>
    )
  return (
    <RadioGroup
      aria-label="Way in"
      aria-busy={loading || undefined}
      value={value}
      onValueChange={onChange}
      className={cn('grid gap-3', loading && 'opacity-60')}
    >
      {plan.options.map((option) => (
        <OptionCard
          key={option.kind}
          option={option}
          selected={option.kind === value}
        />
      ))}
    </RadioGroup>
  )
}

function OptionCard({
  option,
  selected,
}: {
  option: PlanOption
  selected: boolean
}) {
  return (
    <label
      className={cn(
        'flex cursor-pointer items-start gap-3 rounded-md border-2 p-3',
        selected ? 'border-primary' : 'border-border hover:border-primary/50'
      )}
    >
      <RadioGroupItem value={option.kind} className="mt-1" />
      <div className="min-w-0 flex-1 space-y-1">
        <p className="font-medium">
          {option.title}
          {option.recommended && (
            <span className="ml-2 text-xs font-normal text-muted-foreground">
              Recommended
            </span>
          )}
        </p>
        <p className="text-sm text-muted-foreground">{option.description}</p>
        {selected && (
          <ol className="list-decimal space-y-0.5 pl-5 pt-1 text-xs text-muted-foreground">
            {option.steps.map((step) => (
              <li key={`${step.kind}-${step.label}`}>{step.label}</li>
            ))}
          </ol>
        )}
        {option.warnings.map((warning) => (
          <p key={warning} className="text-xs text-warning-strong">
            {warning}
          </p>
        ))}
      </div>
    </label>
  )
}

function SetupFieldInput({
  field: setupField,
  control,
  zone,
  token,
}: {
  field: SetupField
  control: ReturnType<typeof useForm<Values>>['control']
  zone: string
  token: string
}) {
  const providerType =
    setupField.kind.type === 'dns_provider'
      ? setupField.kind.provider_type
      : null
  const providers = useQuery({
    ...listDnsProvidersOptions(),
    enabled: setupField.kind.type === 'dns_provider',
  })
  const choices = (providers.data ?? []).filter(
    (p) => p.is_active && (!providerType || p.provider_type === providerType)
  )
  return (
    <FormField
      control={control}
      name={`values.${setupField.key}`}
      render={({ field }) => (
        <FormItem>
          <FormLabel>
            {setupField.label}
            {!setupField.required && (
              <span className="ml-1 font-normal text-muted-foreground">
                (optional)
              </span>
            )}
          </FormLabel>
          {setupField.kind.type === 'dns_provider' ? (
            providers.isPending ? (
              <Skeleton className="h-9 w-full" />
            ) : (
              <Select value={field.value ?? ''} onValueChange={field.onChange}>
                <FormControl>
                  <SelectTrigger>
                    <SelectValue placeholder="Choose an account" />
                  </SelectTrigger>
                </FormControl>
                <SelectContent>
                  {choices.map((provider) => (
                    <SelectItem key={provider.id} value={String(provider.id)}>
                      {provider.name}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            )
          ) : (
            <FormControl>
              <Input
                type={setupField.kind.type === 'secret' ? 'password' : 'text'}
                autoComplete="off"
                placeholder={setupField.placeholder ?? undefined}
                {...field}
                value={field.value ?? ''}
              />
            </FormControl>
          )}
          {setupField.help && (
            <FormDescription>{setupField.help}</FormDescription>
          )}
          {setupField.key === 'cloudflare_api_token' && (
            <CloudflareTokenCheck token={token} zone={zoneOf(zone)} />
          )}
          <FormMessage />
        </FormItem>
      )}
    />
  )
}

/** The zone a hostname is most likely in: its last two labels. */
function zoneOf(address: string): string | null {
  const labels = address.trim().toLowerCase().split('.').filter(Boolean)
  return labels.length >= 2 ? labels.slice(-2).join('.') : null
}

const stepIcons = {
  pending: Circle,
  running: CircleDot,
  done: CheckCircle2,
  failed: XCircle,
  skipped: MinusCircle,
} as const

const stepTone = {
  pending: 'text-muted-foreground',
  running: 'text-primary animate-pulse',
  done: 'text-emerald-700 dark:text-emerald-400',
  failed: 'text-destructive',
  skipped: 'text-muted-foreground',
} as const

function RunProgress({
  run,
  loadError,
  undoing,
  undoError,
  onUndo,
  resuming,
  resumeError,
  onResume,
  deciding,
  decideError,
  onDecide,
  onStartOver,
}: {
  run: RunView | undefined
  loadError: string | null
  undoing: boolean
  undoError: string | null
  onUndo: () => void
  resuming: boolean
  resumeError: string | null
  onResume: (token: string | null) => void
  deciding: boolean
  decideError: string | null
  onDecide: (key: string, decision: Decision) => void
  onStartOver: () => void
}) {
  // The token, when resuming needs it again; it is never stored before the
  // run saves it.
  const [token, setToken] = useState('')
  if (loadError)
    return (
      <Alert variant="destructive">
        <AlertDescription>{loadError}</AlertDescription>
      </Alert>
    )
  if (!run)
    return (
      <div className="space-y-2">
        <Skeleton className="h-5 w-2/3" />
        <Skeleton className="h-5 w-1/2" />
        <Skeleton className="h-5 w-3/5" />
      </div>
    )
  const undone = run.undo.filter((u) => u.ok)
  const undoFailed = run.undo.filter((u) => !u.ok)
  const resumable = canResumeRun(run)
  const needsToken = resumable && resumeNeedsToken(run)
  const conflicts = conflictsToDecide(run)
  const undoPaused = run.state === 'undo_paused'
  return (
    <div className="space-y-4">
      {run.state === 'recovering' && (
        <Alert variant="warning">
          <AlertTriangle className="h-4 w-4" />
          <AlertDescription className="space-y-1">
            <p className="font-medium">Recovering an interrupted setup</p>
            <p>
              {run.error ??
                'Temps restarted during this setup and is checking what its unfinished steps made. Resume and undo come back once it has.'}
            </p>
          </AlertDescription>
        </Alert>
      )}
      <ol className="space-y-2" aria-label="Progress">
        {run.steps.map((step: RunStep, index) => {
          const Icon = stepIcons[step.status]
          return (
            <li key={`${step.kind}-${index}`} className="flex gap-2 text-sm">
              <Icon
                aria-hidden="true"
                className={cn('mt-0.5 size-4 shrink-0', stepTone[step.status])}
              />
              <div className="min-w-0">
                <p>
                  {step.label}
                  <span className="sr-only"> ({step.status})</span>
                </p>
                {step.detail && (
                  <p className="text-xs text-muted-foreground">{step.detail}</p>
                )}
              </div>
            </li>
          )
        })}
      </ol>
      {run.state === 'succeeded' && run.live_url && (
        <Alert>
          <CheckCircle2 className="h-4 w-4" />
          <AlertDescription className="flex flex-wrap items-center gap-2">
            Live at
            <a
              href={addressUrl(run.live_url)}
              target="_blank"
              rel="noreferrer"
              className="inline-flex items-center gap-1 font-medium underline"
            >
              {run.live_url}
              <ExternalLink className="h-3 w-3" />
            </a>
            <CopyButton value={addressUrl(run.live_url)} minimal />
          </AlertDescription>
        </Alert>
      )}
      {run.error && run.state !== 'undone' && run.state !== 'recovering' && (
        <Alert
          variant={
            run.state === 'waiting' || run.state === 'interrupted'
              ? 'warning'
              : 'destructive'
          }
        >
          <AlertTriangle className="h-4 w-4" />
          <AlertDescription>{run.error}</AlertDescription>
        </Alert>
      )}
      {run.state === 'waiting' && !run.error && (
        <p className="text-sm text-muted-foreground">
          Everything is set up; Temps keeps checking until the address answers.
        </p>
      )}
      {undone.length > 0 && (
        <div className="text-sm">
          <p className="font-medium">
            {run.state === 'failed' ? 'Removed again' : 'Undone'}
          </p>
          <ul className="list-disc pl-5 text-muted-foreground">
            {undone.map((u) => (
              <li key={`${u.label}-${u.resource.type}`}>{u.label}</li>
            ))}
          </ul>
        </div>
      )}
      {undoFailed.length > 0 && (
        <Alert variant="destructive">
          <AlertDescription>
            <p>Could not remove:</p>
            <ul className="list-disc pl-5">
              {undoFailed.map((u) => (
                <li key={`${u.label}-${u.resource.type}`}>
                  {u.label}: {u.error}
                </li>
              ))}
            </ul>
          </AlertDescription>
        </Alert>
      )}
      {undoError && (
        <Alert variant="destructive">
          <AlertDescription>{undoError}</AlertDescription>
        </Alert>
      )}
      {conflicts.length > 0 && (
        <div className="space-y-3 rounded-md border p-3">
          <p className="text-sm font-medium">
            {undoPaused
              ? 'Undo is waiting for you'
              : 'Someone else changed something this setup works on'}
          </p>
          {conflicts.map((entry) => (
            <div key={entry.key} className="space-y-2 text-sm">
              <p>
                {conflictSubject(entry)}
                {entry.intent.type === 'connector'
                  ? ' also publishes '
                  : ' is now '}
                <span className="font-medium">
                  {entry.conflict ?? 'something else'}
                </span>
                .{' '}
                <span className="text-muted-foreground">
                  Temps leaves it as it is until you decide.
                </span>
              </p>
              <div className="flex flex-wrap gap-2">
                {conflictChoices(entry, undoPaused).map((choice) => (
                  <Button
                    key={choice.decision}
                    size="sm"
                    variant={
                      choice.decision === 'keep' ? 'outline' : 'secondary'
                    }
                    disabled={deciding}
                    onClick={() => onDecide(entry.key, choice.decision)}
                  >
                    {choice.label}
                  </Button>
                ))}
              </div>
            </div>
          ))}
        </div>
      )}
      {decideError && (
        <Alert variant="destructive">
          <AlertDescription>{decideError}</AlertDescription>
        </Alert>
      )}
      {needsToken && (
        <div className="space-y-2">
          <label className="text-sm font-medium" htmlFor="resume-token">
            Cloudflare API token
          </label>
          <Input
            id="resume-token"
            type="password"
            autoComplete="off"
            value={token}
            onChange={(event) => setToken(event.target.value)}
          />
          <p className="text-xs text-muted-foreground">
            Paste it again: Temps never stores it until the run saves the
            account.
          </p>
          <CloudflareTokenCheck
            token={token}
            zone={zoneOf(run.address ?? '')}
          />
        </div>
      )}
      {resumeError && (
        <Alert variant="destructive">
          <AlertDescription>{resumeError}</AlertDescription>
        </Alert>
      )}
      <div className="flex flex-wrap gap-2">
        {resumable && (
          <Button
            onClick={() => onResume(needsToken ? token.trim() : null)}
            disabled={resuming || (needsToken && !token.trim())}
          >
            Resume
          </Button>
        )}
        {canUndoRun(run) && (
          <Button variant="outline" onClick={onUndo} disabled={undoing}>
            Undo
          </Button>
        )}
        {!isActiveRun(run.state) && run.state !== 'succeeded' && (
          <Button variant="ghost" onClick={onStartOver}>
            Start over
          </Button>
        )}
      </div>
    </div>
  )
}
