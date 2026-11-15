// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// *Make it reachable* as a page (ADR 069): who can open it, at which
// address and how visitors get in, exactly what Temps will do, and the run
// that does it. The backend says first which audiences can work for this
// environment, so the user only picks among options that do; the URL holds
// the choices and the run, so every entry point links here and a run can be
// left and found again.

import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  AlertTriangle,
  ArrowRight,
  CheckCircle2,
  Circle,
  Loader2,
} from 'lucide-react'
import { useEffect, useMemo, type ReactNode } from 'react'
import { useForm, useWatch } from 'react-hook-form'
import {
  Link,
  Navigate,
  useLocation,
  useParams,
  useSearchParams,
} from 'react-router'
import {
  planEnvironmentReachability,
  type AudienceChoice,
  type EnvironmentResponse,
  type PlanBlocked,
  type PlanOption,
  type ProjectResponse,
  type ReachAction,
} from '@/api/client'
import {
  applyEnvironmentReachabilityMutation,
  getEnvironmentReachabilityOptions,
  getEnvironmentsOptions,
  getReachabilityChoicesOptions,
  listReachabilityRunsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { PageHeader } from '@/components/layout/PageContainer'
import { ReachabilityRun } from '@/components/reachability/ReachabilityRun'
import {
  reachabilityFormSchema,
  type ReachabilityFormValues,
} from '@/components/reachability/reachability-form'
import { SetupFieldInput } from '@/components/reachability/SetupFieldInput'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { CopyButton } from '@/components/ui/copy-button'
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
import { Skeleton } from '@/components/ui/skeleton'
import { Textarea } from '@/components/ui/textarea'
import { useDebounce } from '@/hooks/useDebounce'
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'
import { problemDetail } from '@/lib/api-problem'
import {
  accessPolicyMissing,
  applyValues,
  AUDIENCE_LABELS,
  defaultOption,
  missingAnswers,
  parsePrincipals,
  planIsCurrent,
  privateDnsWarning,
  SIGN_IN_GATES,
  stalePlan,
  wayInLabel,
} from '@/lib/reachability'
import {
  addressesLeftAlone,
  audienceCards,
  dnsHandling,
  readPageParams,
  resolveEnvironmentParam,
  RUN_STATE_LABELS,
  selectAudience,
  unfinishedRun,
  writePageParams,
  type AudienceCard,
  type PageAudience,
  type ReachabilityPageParams,
} from '@/lib/reachability-page'
import { cn } from '@/lib/utils'

const AUDIENCE_COPY: Record<PageAudience, { label: string; help: string }> = {
  public: {
    label: 'Anyone on the internet',
    help: 'A public address with HTTPS.',
  },
  signed_in: {
    label: 'People who sign in',
    help: 'A public address with a sign-in in front: the Temps sign-in, or Cloudflare Access.',
  },
  private: {
    label: 'Only my private network',
    help: 'Served by a worker on your tailnet or LAN, or at its own tailnet name with Tailscale Serve; nobody else can connect.',
  },
  tailnet_users: {
    label: 'Only these tailnet users',
    help: 'Private on your tailnet, and admitted only for the logins and tags you list.',
  },
}

export function ReachabilityPage({ project }: { project: ProjectResponse }) {
  const { environment: environmentParam } = useParams()
  const location = useLocation()
  const environments = useQuery({
    ...getEnvironmentsOptions({ path: { project_id: project.id } }),
  })
  const environment = resolveEnvironmentParam(
    environmentParam,
    environments.data ?? []
  )

  if (environments.isPending)
    return (
      <div className="w-full min-w-0 space-y-6" aria-busy="true">
        <Skeleton className="h-8 w-64" />
        <Skeleton className="h-32 w-full" />
        <Skeleton className="h-32 w-full" />
      </div>
    )
  if (environments.isError)
    return (
      <Alert variant="destructive">
        <AlertDescription className="flex flex-wrap items-center gap-2">
          {problemDetail(
            environments.error,
            'Could not load the environments.'
          )}
          <Button
            size="sm"
            variant="outline"
            onClick={() => void environments.refetch()}
          >
            Retry
          </Button>
        </AlertDescription>
      </Alert>
    )
  if (!environment)
    return (
      <div className="w-full min-w-0 space-y-6">
        <PageHeader title="Make it reachable" />
        <Alert variant="warning">
          <AlertTriangle className="h-4 w-4" />
          <AlertDescription className="space-y-2">
            <p>
              {project.name} has no environment called “{environmentParam}”.
            </p>
            <p className="flex flex-wrap gap-2">
              {(environments.data ?? []).map((env) => (
                <Link
                  key={env.id}
                  className="underline"
                  to={`/projects/${project.slug}/environments/${env.id}/reachability${location.search}`}
                >
                  {env.name}
                </Link>
              ))}
            </p>
          </AlertDescription>
        </Alert>
      </div>
    )
  // A hand-typed name works; the address bar then shows the id every link
  // uses, keeping the query.
  if (environmentParam !== String(environment.id))
    return (
      <Navigate
        replace
        to={`/projects/${project.slug}/environments/${environment.id}/reachability${location.search}`}
      />
    )
  return (
    <ReachabilityFlow
      key={environment.id}
      project={project}
      environment={environment}
    />
  )
}

function ReachabilityFlow({
  project,
  environment,
}: {
  project: ProjectResponse
  environment: EnvironmentResponse
}) {
  const [search, setSearch] = useSearchParams()
  const params = readPageParams(search)
  const path = { project_id: project.id, environment_id: environment.id }
  const domainsHref = `/projects/${project.slug}/domains`
  const update = (
    change: Partial<ReachabilityPageParams>,
    options: { replace?: boolean } = {}
  ) => setSearch(writePageParams(search, change), options)

  const runs = useQuery({
    ...listReachabilityRunsOptions({ path }),
    refetchInterval: 15_000,
  })
  const pending = unfinishedRun(runs.data)

  return (
    <div className="w-full min-w-0 space-y-6">
      <PageHeader
        title="Make it reachable"
        description={`Give ${environment.name} an address, for the people who should open it. Temps sets up only what is missing, and can undo it.`}
        actions={
          <Button variant="outline" asChild>
            <Link to={domainsHref}>Who can reach it</Link>
          </Button>
        }
      />
      {params.run !== null ? (
        <section aria-labelledby="run-heading" className="space-y-3">
          <h2 id="run-heading" className="text-lg font-semibold">
            Run
          </h2>
          <ReachabilityRun
            projectId={project.id}
            environmentId={environment.id}
            runId={params.run}
            onStartOver={() => update({ run: null })}
          />
        </section>
      ) : (
        <>
          {pending && (
            <Alert variant="warning">
              <AlertTriangle className="h-4 w-4" />
              <AlertTitle>
                A setup for {environment.name} is not finished:{' '}
                {RUN_STATE_LABELS[pending.state].toLowerCase()}
              </AlertTitle>
              <AlertDescription className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
                <span>
                  {pending.address ?? 'Its address'} ·{' '}
                  {AUDIENCE_LABELS[pending.audience]}. Watch it, resume it or
                  undo it before starting another.
                </span>
                <Button size="sm" className="shrink-0" asChild>
                  <Link to={`?${writePageParams(search, { run: pending.id })}`}>
                    Open the setup
                  </Link>
                </Button>
              </AlertDescription>
            </Alert>
          )}
          <PlanForm
            project={project}
            environment={environment}
            params={params}
            update={update}
          />
        </>
      )}
    </div>
  )
}

function PlanForm({
  project,
  environment,
  params,
  update,
}: {
  project: ProjectResponse
  environment: EnvironmentResponse
  params: ReachabilityPageParams
  update: (
    change: Partial<ReachabilityPageParams>,
    options?: { replace?: boolean }
  ) => void
}) {
  const queryClient = useQueryClient()
  const path = { project_id: project.id, environment_id: environment.id }
  const { handleSensitiveActionError, verificationDialog } =
    useSensitiveActionVerification()

  // 1. What can work here, before the user chooses (ADR 069 D3).
  const choices = useQuery({
    ...getReachabilityChoicesOptions({ path }),
    staleTime: 30_000,
    retry: false,
  })
  const reachability = useQuery({
    ...getEnvironmentReachabilityOptions({ path }),
    staleTime: 30_000,
    retry: false,
  })
  const cards = useMemo(
    () => audienceCards(choices.data?.audiences ?? []),
    [choices.data]
  )
  const selection = selectAudience(cards, params)
  const audience = selection.audience
  const gate = selection.gate

  const form = useForm<ReachabilityFormValues>({
    resolver: zodResolver(reachabilityFormSchema),
    defaultValues: {
      address: params.address,
      principals: '',
      option: '',
      values: {},
      confirmed: false,
    },
  })
  const typedAddress = useWatch({ control: form.control, name: 'address' })
  const requested = useDebounce(typedAddress.trim(), 500)
  const chosen = useWatch({ control: form.control, name: 'option' })
  const answers = useWatch({ control: form.control, name: 'values' })
  const typedPrincipals = useWatch({
    control: form.control,
    name: 'principals',
  })
  const listed = parsePrincipals(useDebounce(typedPrincipals, 500))
  const principals = audience === 'tailnet_users' ? listed.principals : []

  // The address travels in the URL as typed, without a history entry per
  // keystroke (ADR 069 D2).
  useEffect(() => {
    if (requested !== params.address)
      update({ address: requested }, { replace: true })
    // Only the settled address drives this; `update` is rebuilt each render.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [requested])

  // 2. The plan for the address typed now.
  const planKey = [
    'reachabilityPlan',
    project.id,
    environment.id,
    audience,
    gate,
    requested,
    principals.map((p) => p.value).join(','),
  ]
  const plan = useQuery({
    queryKey: planKey,
    queryFn: async ({ signal }) => {
      const { data } = await planEnvironmentReachability({
        path,
        body: {
          audience: audience ?? 'public',
          address: requested || null,
          gate,
          ...(audience === 'tailnet_users' ? { principals } : {}),
        },
        signal,
        throwOnError: true,
      })
      return data
    },
    enabled: audience !== null,
    placeholderData: (previous) => previous,
    retry: false,
  })
  const planCurrent = planIsCurrent({
    typed: typedAddress,
    requested,
    placeholder: plan.isPlaceholderData,
  })

  // The suggestion fills an empty address once.
  const suggested = plan.data?.address
  useEffect(() => {
    if (suggested && !form.getValues('address'))
      form.setValue('address', suggested)
  }, [suggested, form])
  const options = useMemo(() => plan.data?.options ?? [], [plan.data])
  useEffect(() => {
    if (!options.some((o) => o.kind === form.getValues('option'))) {
      const preferred = options.find((o) => o.kind === params.way)
      form.setValue('option', (preferred ?? defaultOption(options))?.kind ?? '')
      form.setValue('confirmed', false)
    }
  }, [options, form, params.way])
  const option = options.find((o) => o.kind === chosen)
  const fields = (option?.fields ?? []).filter((f) => f.key !== 'address')

  const apply = useMutation({
    ...applyEnvironmentReachabilityMutation(),
    // The run's URL is its own history entry: Back returns to this plan.
    onSuccess: (started) => update({ run: started.id }),
    onError: (error, variables) => {
      if (handleSensitiveActionError(error, () => apply.mutate(variables)))
        return
      const fresh = stalePlan(error)
      if (fresh) {
        queryClient.setQueryData(planKey, fresh)
        form.setValue('confirmed', false)
      }
    },
  })

  const submit = (values: ReachabilityFormValues) => {
    if (!option) return
    const missing = missingAnswers(fields, values.values)
    if (missing.length > 0) {
      form.setError('values', {
        message: `Still needed: ${missing.join(', ')}.`,
      })
      return
    }
    const noOne = accessPolicyMissing(fields, values.values)
    if (noOne) {
      form.setError('values', { message: noOne })
      return
    }
    if (audience === 'tailnet_users') {
      const typed = parsePrincipals(values.principals)
      if (typed.invalid.length > 0) {
        form.setError('values', {
          message: `Not a login or a tag: ${typed.invalid.join(', ')}.`,
        })
        return
      }
      const key = (list: { kind: string; value: string }[]) =>
        list.map((p) => `${p.kind}:${p.value}`).join(',')
      if (key(typed.principals) !== key(plan.data?.principals ?? [])) {
        form.setError('values', {
          message: 'The plan is still being worked out. Try again in a moment.',
        })
        return
      }
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

  const chooseAudience = (next: PageAudience) => {
    if (next === audience) return
    form.setValue('option', '')
    form.setValue('confirmed', false)
    // Each audience gets its own suggestion.
    if (form.getValues('address') === suggested) form.setValue('address', '')
    update({ audience: next, gate: null, way: null })
  }
  const chooseGate = (next: string) => {
    form.setValue('option', '')
    form.setValue('confirmed', false)
    update({
      audience: 'signed_in',
      gate: next as 'temps' | 'cloudflare_access',
    })
  }

  const choice = selection.choice
  const leftAlone = addressesLeftAlone(
    reachability.data?.addresses ?? [],
    plan.data?.address ?? (typedAddress.trim() || null)
  )

  return (
    <>
      <Form {...form}>
        <form
          onSubmit={form.handleSubmit(submit)}
          className="space-y-8"
          aria-label="Make it reachable"
        >
          {/* 1. Who can open it */}
          <section aria-labelledby="audience-heading" className="space-y-3">
            <SectionHeading id="audience-heading" step={1}>
              Who can open it
            </SectionHeading>
            {choices.isPending ? (
              <div className="grid gap-3 md:grid-cols-2">
                {[0, 1, 2, 3].map((i) => (
                  <Skeleton key={i} className="h-24 w-full" />
                ))}
              </div>
            ) : choices.isError ? (
              <Alert variant="destructive">
                <AlertDescription className="flex flex-wrap items-center gap-2">
                  {problemDetail(
                    choices.error,
                    `Could not work out what can reach ${environment.name}.`
                  )}
                  <Button
                    size="sm"
                    variant="outline"
                    onClick={() => void choices.refetch()}
                  >
                    Retry
                  </Button>
                </AlertDescription>
              </Alert>
            ) : (
              <>
                {selection.refused && (
                  <Alert variant="warning">
                    <AlertTriangle className="h-4 w-4" />
                    <AlertDescription>
                      The link asked for “
                      {AUDIENCE_COPY[selection.refused.audience].label}”, which
                      cannot work for {environment.name} yet:{' '}
                      {selection.refused.blocked?.reason ??
                        'no way in fits it.'}
                    </AlertDescription>
                  </Alert>
                )}
                {audience === null && (
                  <Alert variant="warning">
                    <AlertTriangle className="h-4 w-4" />
                    <AlertDescription>
                      Nothing can make {environment.name} reachable yet. Each
                      option below says what is missing.
                    </AlertDescription>
                  </Alert>
                )}
                <RadioGroup
                  aria-label="Who can open it"
                  value={audience ?? ''}
                  onValueChange={(next) => chooseAudience(next as PageAudience)}
                  className="grid gap-3 md:grid-cols-2"
                >
                  {cards.map((card) => (
                    <AudienceCardView
                      key={card.audience}
                      card={card}
                      selected={card.audience === audience}
                    />
                  ))}
                </RadioGroup>
                {audience === 'signed_in' && (
                  <GateChoice
                    entries={
                      cards.find((c) => c.audience === 'signed_in')?.entries ??
                      []
                    }
                    value={gate ?? 'temps'}
                    onChange={chooseGate}
                  />
                )}
              </>
            )}
          </section>

          {/* 2. Address and how visitors get in */}
          <section aria-labelledby="address-heading" className="space-y-4">
            <SectionHeading id="address-heading" step={2}>
              Address and how visitors get in
            </SectionHeading>
            {audience === null ? (
              <p className="text-sm text-muted-foreground">
                Choose who can open it first.
              </p>
            ) : (
              <>
                {audience === 'tailnet_users' && (
                  <FormField
                    control={form.control}
                    name="principals"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Tailnet users and tags</FormLabel>
                        <FormControl>
                          <Textarea
                            autoComplete="off"
                            rows={2}
                            placeholder="alice@example.com, tag:ci"
                            {...field}
                          />
                        </FormControl>
                        <FormDescription>
                          Logins as Tailscale shows them, or a device tag such
                          as <code>tag:server</code>, separated by commas or new
                          lines. Everyone else on the tailnet is refused.
                        </FormDescription>
                        {listed.invalid.length > 0 && (
                          <p className="text-sm text-destructive">
                            Not a login or a tag: {listed.invalid.join(', ')}
                          </p>
                        )}
                      </FormItem>
                    )}
                  />
                )}
                <FormField
                  control={form.control}
                  name="address"
                  render={({ field }) => (
                    <FormItem className="max-w-xl">
                      <FormLabel>Address</FormLabel>
                      <FormControl>
                        <Input
                          autoComplete="off"
                          placeholder={
                            choice?.suggested_address ?? 'myapp.example.com'
                          }
                          {...field}
                        />
                      </FormControl>
                      <FormDescription>
                        {audience === 'private' || audience === 'tailnet_users'
                          ? 'The name people on your network will open: a tailnet name, or a hostname on a domain you own.'
                          : 'The hostname people will open, on a domain you own.'}
                      </FormDescription>
                      {plan.data?.address_error && (
                        <p className="text-sm text-destructive">
                          {plan.data.address_error}
                        </p>
                      )}
                    </FormItem>
                  )}
                />
                <WaysIn
                  options={options}
                  loading={plan.isPending || !planCurrent}
                  error={
                    plan.isError
                      ? problemDetail(
                          plan.error,
                          'Could not work out a plan. Check the address and try again.'
                        )
                      : null
                  }
                  blocked={plan.data?.blocked ?? null}
                  value={chosen}
                  onChange={(kind) => {
                    form.setValue('option', kind)
                    form.setValue('confirmed', false)
                    update(
                      { way: kind as PlanOption['kind'] },
                      { replace: true }
                    )
                  }}
                />
                {choice && choice.not_offered.length > 0 && (
                  <div className="space-y-1 text-sm text-muted-foreground">
                    <p className="font-medium text-foreground">Not offered</p>
                    {choice.not_offered.map((note) => (
                      <BlockedReason key={note.reason} blocked={note} />
                    ))}
                  </div>
                )}
                {option && fields.length > 0 && (
                  <div className="max-w-xl space-y-4">
                    {fields.map((setupField) => (
                      <SetupFieldInput
                        key={setupField.key}
                        field={setupField}
                        control={form.control}
                        address={typedAddress}
                        token={answers[setupField.key] ?? ''}
                      />
                    ))}
                  </div>
                )}
              </>
            )}
          </section>

          {/* 3. Plan */}
          <section aria-labelledby="plan-heading" className="space-y-4">
            <SectionHeading id="plan-heading" step={3}>
              Plan
            </SectionHeading>
            {option ? (
              <div className="grid gap-6 lg:grid-cols-2">
                <div className="min-w-0 space-y-2">
                  <h3 className="text-sm font-medium">What Temps will do</h3>
                  <ol className="space-y-2" aria-label="What Temps will do">
                    {option.steps.map((step, index) => (
                      <li
                        key={`${step.kind}-${index}`}
                        className="flex gap-2 text-sm"
                      >
                        <span
                          aria-hidden="true"
                          className="mt-0.5 flex size-5 shrink-0 items-center justify-center rounded-full border text-xs text-muted-foreground"
                        >
                          {index + 1}
                        </span>
                        <span className="min-w-0 break-words">
                          {step.label}
                        </span>
                      </li>
                    ))}
                  </ol>
                  {option.warnings.map((warning) => (
                    <p key={warning} className="text-sm text-warning-strong">
                      {warning}
                    </p>
                  ))}
                </div>
                <div className="min-w-0 space-y-2">
                  <h3 className="text-sm font-medium">What it leaves alone</h3>
                  <ul className="space-y-2 text-sm text-muted-foreground">
                    {leftAlone.map((a) => (
                      <li key={a.address} className="flex gap-2">
                        <Circle
                          aria-hidden="true"
                          className="mt-1 size-3 shrink-0"
                        />
                        <span className="min-w-0 break-all">
                          {a.address}{' '}
                          <span className="whitespace-nowrap">
                            ({AUDIENCE_LABELS[a.audience]}), as it is
                          </span>
                        </span>
                      </li>
                    ))}
                    <li className="flex gap-2">
                      <Circle
                        aria-hidden="true"
                        className="mt-1 size-3 shrink-0"
                      />
                      Every DNS record, tunnel and certificate not listed on the
                      left.
                    </li>
                    <li className="flex gap-2">
                      <Circle
                        aria-hidden="true"
                        className="mt-1 size-3 shrink-0"
                      />
                      Anything already there is used as found; undo never
                      removes it, and a record someone else changes is never
                      overwritten without asking you.
                    </li>
                  </ul>
                </div>
              </div>
            ) : (
              <p className="text-sm text-muted-foreground">
                Pick a way in to see exactly what Temps will create.
              </p>
            )}
          </section>

          {/* 4. Run */}
          <section aria-labelledby="start-heading" className="space-y-4">
            <SectionHeading id="start-heading" step={4}>
              Run
            </SectionHeading>
            {option?.confirmation && (
              <FormField
                control={form.control}
                name="confirmed"
                render={({ field }) => (
                  <FormItem className="flex items-start gap-3 space-y-0">
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
                <AlertDescription>{privateDnsWarning(option)}</AlertDescription>
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
            <p className="text-sm text-muted-foreground">
              The setup runs on the server. You can leave this page while it
              runs; it gets its own link, and{' '}
              <Link
                className="underline"
                to={`/projects/${project.slug}/domains`}
              >
                Who can reach it
              </Link>{' '}
              links back to it.
            </p>
            <Button
              type="submit"
              disabled={!option || !planCurrent || apply.isPending}
            >
              {(apply.isPending || (option && !planCurrent)) && (
                <Loader2 className="mr-2 h-4 w-4 animate-spin" />
              )}
              {option ? option.title : 'Make it reachable'}
            </Button>
          </section>
        </form>
      </Form>
      {verificationDialog}
    </>
  )
}

function SectionHeading({
  id,
  step,
  children,
}: {
  id: string
  step: number
  children: ReactNode
}) {
  return (
    <h2 id={id} className="flex items-center gap-2 text-lg font-semibold">
      <span
        aria-hidden="true"
        className="flex size-6 items-center justify-center rounded-full bg-muted text-sm"
      >
        {step}
      </span>
      {children}
    </h2>
  )
}

function AudienceCardView({
  card,
  selected,
}: {
  card: AudienceCard
  selected: boolean
}) {
  const copy = AUDIENCE_COPY[card.audience]
  const id = `audience-${card.audience}`
  return (
    <div
      className={cn(
        'flex items-start gap-3 rounded-md border p-4',
        selected ? 'border-primary' : 'border-border',
        card.available
          ? 'hover:border-primary/50'
          : 'bg-muted/40 text-muted-foreground'
      )}
    >
      <RadioGroupItem
        id={id}
        value={card.audience}
        disabled={!card.available}
        aria-describedby={`${id}-help`}
        className="mt-0.5"
      />
      <div className="min-w-0 space-y-1">
        <label
          htmlFor={id}
          className={cn(
            'block text-sm font-medium',
            card.available ? 'cursor-pointer' : 'cursor-not-allowed'
          )}
        >
          {copy.label}
        </label>
        <p id={`${id}-help`} className="text-xs text-muted-foreground">
          {card.available ? copy.help : 'Not possible here yet.'}
        </p>
        {!card.available && card.blocked && (
          <BlockedReason blocked={card.blocked} />
        )}
        {card.available && <WaysInSummary entries={card.entries} />}
      </div>
    </div>
  )
}

/** The ways in an audience offers, in a line, before it is chosen. */
function WaysInSummary({ entries }: { entries: AudienceChoice[] }) {
  const ways = [
    ...new Set(
      entries.flatMap((e) => e.ways_in.map((w) => wayInLabel(w.way_in)))
    ),
  ]
  if (ways.length === 0) return null
  return (
    <p className="text-xs text-muted-foreground">Ways in: {ways.join(', ')}</p>
  )
}

function GateChoice({
  entries,
  value,
  onChange,
}: {
  entries: AudienceChoice[]
  value: string
  onChange: (gate: string) => void
}) {
  return (
    <div className="space-y-2">
      <p className="text-sm font-medium">Sign in with</p>
      <RadioGroup
        aria-label="Sign in with"
        value={value}
        onValueChange={onChange}
        className="grid gap-3 md:grid-cols-2"
      >
        {SIGN_IN_GATES.map((gate) => {
          const entry = entries.find((e) => e.gate === gate.value)
          const available = entry?.available ?? false
          const id = `gate-${gate.value}`
          return (
            <div
              key={gate.value}
              className={cn(
                'flex items-start gap-3 rounded-md border p-3',
                value === gate.value ? 'border-primary' : 'border-border',
                !available && 'bg-muted/40 text-muted-foreground'
              )}
            >
              <RadioGroupItem
                id={id}
                value={gate.value}
                disabled={!available}
                className="mt-0.5"
              />
              <div className="min-w-0 space-y-1">
                <label
                  htmlFor={id}
                  className={cn(
                    'block text-sm font-medium',
                    available ? 'cursor-pointer' : 'cursor-not-allowed'
                  )}
                >
                  {gate.label}
                </label>
                <p className="text-xs text-muted-foreground">{gate.help}</p>
                {!available && entry?.blocked && (
                  <BlockedReason blocked={entry.blocked} />
                )}
              </div>
            </div>
          )
        })}
      </RadioGroup>
    </div>
  )
}

function BlockedReason({ blocked }: { blocked: PlanBlocked }) {
  return (
    <div className="space-y-1 text-xs">
      <p>{blocked.reason}</p>
      {blocked.action && <ActionLink action={blocked.action} />}
    </div>
  )
}

function ActionLink({ action }: { action: ReachAction }) {
  if (action.command)
    return (
      <div className="flex flex-wrap items-center gap-2">
        <span>{action.label}</span>
        <code className="break-all rounded bg-muted px-1.5 py-0.5 text-foreground">
          {action.command}
        </code>
        <CopyButton value={action.command} minimal />
      </div>
    )
  if (action.href)
    return (
      <Link
        className="inline-flex items-center gap-1 text-foreground underline"
        to={action.href}
      >
        {action.label}
        <ArrowRight aria-hidden="true" className="size-3" />
      </Link>
    )
  return <p>{action.label}</p>
}

function WaysIn({
  options,
  loading,
  error,
  blocked,
  value,
  onChange,
}: {
  options: PlanOption[]
  loading: boolean
  error: string | null
  blocked: PlanBlocked | null
  value: string
  onChange: (kind: string) => void
}) {
  if (loading && options.length === 0 && !blocked && !error)
    return (
      <div className="space-y-2" aria-busy="true">
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
  if (options.length === 0)
    return (
      <Alert variant="warning">
        <AlertTriangle className="h-4 w-4" />
        <AlertDescription className="space-y-2">
          <BlockedReason
            blocked={blocked ?? { reason: 'No way in fits this address yet.' }}
          />
        </AlertDescription>
      </Alert>
    )
  return (
    <RadioGroup
      aria-label="How visitors get in"
      aria-busy={loading || undefined}
      value={value}
      onValueChange={onChange}
      className={cn('grid gap-3 lg:grid-cols-2', loading && 'opacity-60')}
    >
      {options.map((option) => {
        const id = `way-${option.kind}`
        return (
          <div
            key={option.kind}
            className={cn(
              'flex items-start gap-3 rounded-md border p-3',
              option.kind === value
                ? 'border-primary'
                : 'border-border hover:border-primary/50'
            )}
          >
            <RadioGroupItem id={id} value={option.kind} className="mt-1" />
            <div className="min-w-0 flex-1 space-y-1">
              <label htmlFor={id} className="block cursor-pointer font-medium">
                {option.title}
                {option.recommended && (
                  <span className="ml-2 text-xs font-normal text-muted-foreground">
                    Recommended
                  </span>
                )}
              </label>
              <p className="text-sm text-muted-foreground">
                {option.description}
              </p>
              <p className="flex items-center gap-1 text-xs text-muted-foreground">
                {option.kind === value ? (
                  <CheckCircle2 aria-hidden="true" className="size-3" />
                ) : null}
                {wayInLabel(option.way_in)} · {dnsHandling(option)}
              </p>
            </div>
          </div>
        )
      })}
    </RadioGroup>
  )
}
