// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ProjectResponse } from '@/api/client'
import { PageHeader } from '@/components/layout/PageContainer'
import { Button } from '@/components/ui/button'
import { EmptyState } from '@/components/ui/empty-state'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group'
import { ReadFailure } from '@/components/ui/read-failure'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { Textarea } from '@/components/ui/textarea'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { cn } from '@/lib/utils'
import { Button as BusyButton, Callout } from '@temps-sdk/ds'
import { useQuery } from '@tanstack/react-query'
import { Check, FileQuestion, FlaskConical } from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import {
  Controller,
  useForm,
  useWatch,
  type FieldError,
  type FieldErrors,
  type Resolver,
} from 'react-hook-form'
import { Link, useNavigate, useParams } from 'react-router'
import { toast } from 'sonner'
import { ConditionBuilder } from './ConditionBuilder'
import {
  describeAction,
  describeExpression,
  emptyDraft,
  ruleWarnings,
  validateRule,
} from './expression'
import { ACTIONS, LIMITS } from './fields'
import { edgeRulesPath, useProjectEnvironments } from './shared'
import { edgeRulesOptions, useCreateEdgeRule, useUpdateEdgeRule } from './store'
import { TestRequestSheet, type EnvironmentOption } from './TestRequestSheet'
import type {
  ActionType,
  EdgeRule,
  EdgeRuleDraft,
  RedirectStatus,
  RuleAction,
} from './types'

const ALL_ENVIRONMENTS = 'all'
const DRAFT_ID = '__draft__'

const BLOCK_STATUSES = [
  { value: 403, label: '403 Forbidden' },
  { value: 404, label: '404 Not Found' },
  { value: 401, label: '401 Unauthorized' },
  { value: 400, label: '400 Bad Request' },
  { value: 410, label: '410 Gone' },
  { value: 429, label: '429 Too Many Requests' },
  { value: 451, label: '451 Unavailable For Legal Reasons' },
]

const REDIRECT_STATUSES: Array<{ value: RedirectStatus; label: string }> = [
  { value: 302, label: '302 Found (temporary)' },
  { value: 307, label: '307 Temporary Redirect (keeps method)' },
  { value: 301, label: '301 Moved Permanently' },
  { value: 308, label: '308 Permanent Redirect (keeps method)' },
]

function defaultAction(type: ActionType): RuleAction {
  switch (type) {
    case 'block':
      return { type, status: 403, body: '' }
    case 'redirect':
      return { type, target: '', status: 302 }
    case 'allow':
    case 'challenge':
    case 'log':
      return { type }
  }
}

function toDraft(rule: EdgeRule): EdgeRuleDraft {
  return {
    name: rule.name,
    enabled: rule.enabled,
    mode: rule.mode,
    environmentId: rule.environmentId,
    expression: rule.expression,
    action: rule.action,
  }
}

const resolver: Resolver<EdgeRuleDraft> = async (values) => {
  const issues = validateRule(values)
  if (issues.length === 0) return { values, errors: {} }
  const errors: Record<string, FieldError> = {}
  const nameIssue = issues.find((i) => i.target === 'name')
  if (nameIssue) errors.name = { type: 'validate', message: nameIssue.message }
  const actionIssues = issues.filter((i) => i.target === 'action')
  if (actionIssues.length)
    errors.action = {
      type: 'validate',
      message: actionIssues.map((i) => i.message).join(' '),
    }
  if (issues.some((i) => i.target !== 'name' && i.target !== 'action'))
    errors.expression = {
      type: 'validate',
      message: 'Fix the highlighted conditions.',
    }
  return { values: {}, errors: errors as FieldErrors<EdgeRuleDraft> }
}

interface EdgeRuleEditorProps {
  project: ProjectResponse
}

export function EdgeRuleEditor({ project }: EdgeRuleEditorProps) {
  const { ruleId } = useParams<{ ruleId: string }>()
  const { setBreadcrumbs } = useBreadcrumbs()
  const base = edgeRulesPath(project.slug)
  const rulesQuery = useQuery(edgeRulesOptions(project.id))
  const { environments } = useProjectEnvironments(project.id)
  const rules = useMemo(() => rulesQuery.data ?? [], [rulesQuery.data])
  const existing = ruleId ? rules.find((r) => r.id === ruleId) : undefined
  const missing = !!ruleId && rulesQuery.isSuccess && !existing

  const currentLabel = !ruleId
    ? 'New rule'
    : (existing?.name ?? (missing ? 'Rule not found' : 'Loading…'))
  useEffect(() => {
    setBreadcrumbs([
      { label: 'Projects', href: '/projects' },
      { label: project.slug, href: `/projects/${project.slug}/project` },
      { label: 'Edge rules', href: base },
      { label: currentLabel },
    ])
  }, [base, currentLabel, project.slug, setBreadcrumbs])

  if (ruleId && rulesQuery.isPending) {
    return (
      <div className="w-full min-w-0 space-y-6" aria-busy="true">
        <Skeleton className="h-8 w-64" />
        <Skeleton className="h-40 w-full" />
        <Skeleton className="h-64 w-full" />
      </div>
    )
  }
  if (ruleId && rulesQuery.isError) {
    return (
      <ReadFailure
        resource="edge rule"
        error={rulesQuery.error}
        onRetry={() => void rulesQuery.refetch()}
        retrying={rulesQuery.isFetching}
      />
    )
  }
  if (missing) {
    return (
      <div className="rounded-lg border bg-card text-card-foreground">
        <EmptyState
          size="compact"
          icon={FileQuestion}
          title="Edge rule not found"
          description={`Rule ${ruleId} does not exist in ${project.slug}. It may have been deleted.`}
          action={
            <Button asChild variant="outline">
              <Link to={base}>Back to edge rules</Link>
            </Button>
          }
        />
      </div>
    )
  }

  return (
    <RuleForm
      key={existing?.id ?? 'new'}
      project={project}
      base={base}
      existing={existing}
      rules={rules}
      environments={environments}
    />
  )
}

interface RuleFormProps {
  project: ProjectResponse
  base: string
  existing: EdgeRule | undefined
  rules: EdgeRule[]
  environments: EnvironmentOption[]
}

function RuleForm({
  project,
  base,
  existing,
  rules,
  environments,
}: RuleFormProps) {
  const navigate = useNavigate()
  const createRule = useCreateEdgeRule(project.id)
  const updateRule = useUpdateEdgeRule(project.id)
  const [testOpen, setTestOpen] = useState(false)
  const {
    control,
    register,
    handleSubmit,
    setValue,
    formState: { errors, submitCount, isSubmitting },
  } = useForm<EdgeRuleDraft>({
    defaultValues: existing ? toDraft(existing) : emptyDraft(),
    resolver,
  })
  // useWatch, unlike watch(), returns a stable value React can memoize on.
  const values = useWatch({ control }) as EdgeRuleDraft
  const issues = useMemo(() => validateRule(values), [values])
  const nodeIssues = useMemo(() => {
    const map = new Map<string, string[]>()
    if (submitCount === 0) return map
    for (const issue of issues) {
      if (issue.target === 'name' || issue.target === 'action') continue
      map.set(issue.target, [...(map.get(issue.target) ?? []), issue.message])
    }
    return map
  }, [issues, submitCount])
  const warnings = useMemo(() => ruleWarnings(values), [values])

  // The tester sees the saved list with this draft in its place, so a rule
  // can be tried before it is saved.
  const testRules = useMemo(() => {
    const draftRule: EdgeRule = {
      ...values,
      id: existing?.id ?? DRAFT_ID,
      name: values.name.trim() || 'This rule',
      revision: existing?.revision ?? 0,
      createdAt: existing?.createdAt ?? '',
      updatedAt: existing?.updatedAt ?? '',
    }
    return existing
      ? rules.map((r) => (r.id === existing.id ? draftRule : r))
      : [...rules, draftRule]
  }, [existing, rules, values])

  const onSubmit = handleSubmit(async (draft) => {
    const saved = existing
      ? await updateRule.mutateAsync({
          ruleId: existing.id,
          revision: existing.revision,
          patch: draft,
        })
      : await createRule.mutateAsync(draft)
    toast.success(
      existing ? `Saved "${saved.name}"` : `Created "${saved.name}"`,
      {
        description:
          saved.mode === 'log_only'
            ? 'It records matches without acting on them.'
            : 'It is enforced on matching requests.',
      }
    )
    navigate(base)
  })

  const action = values.action
  const actionError = (errors as Record<string, FieldError | undefined>).action

  return (
    <form
      onSubmit={(e) => void onSubmit(e).catch(() => undefined)}
      className="w-full min-w-0 space-y-6"
      noValidate
    >
      <PageHeader
        title={existing ? existing.name : 'New edge rule'}
        description={
          existing
            ? `Position ${rules.findIndex((r) => r.id === existing.id) + 1} of ${rules.length}. Changes apply as soon as you save.`
            : 'New rules are added at the end of the list. Start in log-only mode to see what a rule would do before it acts.'
        }
        actions={
          <Button
            type="button"
            variant="outline"
            onClick={() => setTestOpen(true)}
          >
            <FlaskConical />
            Test this rule
          </Button>
        }
      />

      <section
        aria-labelledby="rule-basics"
        className="space-y-4 rounded-lg border bg-card p-4 text-card-foreground sm:p-6"
      >
        <h2 id="rule-basics" className="text-lg font-semibold">
          Rule
        </h2>
        <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
          <div className="space-y-1.5">
            <Label htmlFor="rule-name">Name</Label>
            <Input
              id="rule-name"
              maxLength={LIMITS.nameLength}
              placeholder="Block WordPress probes"
              aria-invalid={!!errors.name}
              aria-describedby={errors.name ? 'rule-name-error' : undefined}
              {...register('name')}
            />
            {errors.name && (
              <p id="rule-name-error" className="text-xs text-destructive">
                {errors.name.message}
              </p>
            )}
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="rule-environment">Applies to</Label>
            <Controller
              control={control}
              name="environmentId"
              render={({ field }) => (
                <Select
                  value={
                    field.value === null
                      ? ALL_ENVIRONMENTS
                      : String(field.value)
                  }
                  onValueChange={(v) =>
                    field.onChange(v === ALL_ENVIRONMENTS ? null : Number(v))
                  }
                >
                  <SelectTrigger id="rule-environment">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value={ALL_ENVIRONMENTS}>
                      All environments
                    </SelectItem>
                    {environments.map((env) => (
                      <SelectItem key={env.id} value={String(env.id)}>
                        {env.name}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              )}
            />
          </div>
        </div>
        <fieldset className="space-y-2">
          <legend className="text-sm font-medium">Mode</legend>
          <Controller
            control={control}
            name="mode"
            render={({ field }) => (
              <RadioGroup
                value={field.value}
                onValueChange={field.onChange}
                className="grid grid-cols-1 gap-2 md:grid-cols-2"
              >
                {(
                  [
                    {
                      value: 'log_only',
                      title: 'Log only',
                      text: 'Record what the rule would do without acting. Recommended while you check a new rule.',
                    },
                    {
                      value: 'enforce',
                      title: 'Enforce',
                      text: 'Apply the action to matching requests.',
                    },
                  ] as const
                ).map((option) => (
                  <Label
                    key={option.value}
                    htmlFor={`mode-${option.value}`}
                    className={cn(
                      'flex cursor-pointer items-start gap-3 rounded-md border p-3 font-normal',
                      field.value === option.value && 'border-primary'
                    )}
                  >
                    <RadioGroupItem
                      id={`mode-${option.value}`}
                      value={option.value}
                      className="mt-0.5"
                    />
                    <span className="space-y-0.5">
                      <span className="block text-sm font-medium">
                        {option.title}
                      </span>
                      <span className="block text-sm text-muted-foreground">
                        {option.text}
                      </span>
                    </span>
                  </Label>
                ))}
              </RadioGroup>
            )}
          />
        </fieldset>
      </section>

      <section
        aria-labelledby="rule-when"
        className="space-y-4 rounded-lg border bg-card p-4 text-card-foreground sm:p-6"
      >
        <div>
          <h2 id="rule-when" className="text-lg font-semibold">
            When
          </h2>
          <p className="text-sm text-muted-foreground">
            Patterns use <code className="font-mono">*</code> for any run of
            characters, for example <code className="font-mono">/admin*</code>.
          </p>
        </div>
        <Controller
          control={control}
          name="expression"
          render={({ field }) => (
            <ConditionBuilder
              value={field.value}
              onChange={field.onChange}
              issues={nodeIssues}
            />
          )}
        />
      </section>

      <section
        aria-labelledby="rule-then"
        className="space-y-4 rounded-lg border bg-card p-4 text-card-foreground sm:p-6"
      >
        <h2 id="rule-then" className="text-lg font-semibold">
          Then
        </h2>
        <div
          role="group"
          aria-label="Action"
          className="grid grid-cols-1 gap-2 sm:grid-cols-2 lg:grid-cols-5"
        >
          {ACTIONS.map((meta) => {
            const selected = action.type === meta.type
            return (
              <button
                key={meta.type}
                type="button"
                aria-pressed={selected}
                onClick={() =>
                  !selected &&
                  setValue('action', defaultAction(meta.type), {
                    shouldDirty: true,
                  })
                }
                className={cn(
                  'flex flex-col items-start gap-1 rounded-md border p-3 text-left transition-colors hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring',
                  selected && 'border-primary'
                )}
              >
                <span className="flex w-full items-center justify-between text-sm font-medium">
                  {meta.label}
                  {selected && <Check className="size-4" aria-hidden="true" />}
                </span>
                <span className="text-xs text-muted-foreground">
                  {meta.description}
                </span>
              </button>
            )
          })}
        </div>
        <ActionFields
          action={action}
          onChange={(next) => setValue('action', next, { shouldDirty: true })}
          invalid={!!actionError}
        />
        {actionError && (
          <p className="text-xs text-destructive">{actionError.message}</p>
        )}
      </section>

      <section
        aria-labelledby="rule-summary"
        className="space-y-3 rounded-lg border bg-card p-4 text-card-foreground sm:p-6"
      >
        <h2 id="rule-summary" className="text-lg font-semibold">
          Summary
        </h2>
        <p className="text-sm">
          When {describeExpression(values.expression)}:{' '}
          <span className="font-medium">
            {describeAction(values.action).toLowerCase()}
          </span>
          {values.mode === 'log_only' && (
            <span className="text-muted-foreground">
              {' '}
              (log only: matches are recorded, nothing is blocked)
            </span>
          )}
          .
        </p>
        {warnings.length > 0 && (
          <Callout tone="warning" title="Check before enforcing">
            <ul className="list-disc space-y-1 pl-4">
              {warnings.map((w) => (
                <li key={w}>{w}</li>
              ))}
            </ul>
          </Callout>
        )}
      </section>

      {submitCount > 0 && issues.length > 0 && (
        <p role="alert" className="text-sm text-destructive">
          The rule has {issues.length}{' '}
          {issues.length === 1 ? 'problem' : 'problems'}. Fix the highlighted
          fields to save it.
        </p>
      )}

      <div className="flex flex-wrap justify-end gap-2">
        <Button asChild variant="outline">
          <Link to={base}>Cancel</Link>
        </Button>
        <BusyButton type="submit" busy={isSubmitting} busyLabel="Saving…">
          {existing ? 'Save rule' : 'Create rule'}
        </BusyButton>
      </div>

      <TestRequestSheet
        open={testOpen}
        onOpenChange={setTestOpen}
        rules={testRules}
        environments={environments}
        focusRuleId={existing?.id ?? DRAFT_ID}
      />
    </form>
  )
}

function ActionFields({
  action,
  onChange,
  invalid,
}: {
  action: RuleAction
  onChange: (next: RuleAction) => void
  invalid: boolean
}) {
  switch (action.type) {
    case 'block':
      return (
        <div className="grid grid-cols-1 gap-4 md:grid-cols-[260px_1fr]">
          <div className="space-y-1.5">
            <Label htmlFor="block-status">Response status</Label>
            <Select
              value={String(action.status)}
              onValueChange={(v) => onChange({ ...action, status: Number(v) })}
            >
              <SelectTrigger id="block-status">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {BLOCK_STATUSES.map((s) => (
                  <SelectItem key={s.value} value={String(s.value)}>
                    {s.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <p className="text-xs text-muted-foreground">
              404 hides that the path exists at all.
            </p>
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="block-body">Response body (optional)</Label>
            <Textarea
              id="block-body"
              rows={2}
              maxLength={LIMITS.blockBodyBytes}
              value={action.body}
              placeholder="Forbidden"
              onChange={(e) => onChange({ ...action, body: e.target.value })}
            />
            <p className="text-xs text-muted-foreground">
              Plain text, up to {LIMITS.blockBodyBytes} bytes. Leave empty for a
              short default.
            </p>
          </div>
        </div>
      )
    case 'redirect':
      return (
        <div className="grid grid-cols-1 gap-4 md:grid-cols-[1fr_300px]">
          <div className="space-y-1.5">
            <Label htmlFor="redirect-target">Redirect to</Label>
            <Input
              id="redirect-target"
              value={action.target}
              placeholder="https://docs.example.com{path}"
              className="font-mono"
              aria-invalid={invalid}
              onChange={(e) => onChange({ ...action, target: e.target.value })}
            />
            <p className="text-xs text-muted-foreground">
              A path such as <code className="font-mono">/new</code> or an
              absolute URL. <code className="font-mono">{'{path}'}</code>{' '}
              inserts the visitor&apos;s path and{' '}
              <code className="font-mono">{'{query}'}</code> the query string.
            </p>
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="redirect-status">Status</Label>
            <Select
              value={String(action.status)}
              onValueChange={(v) =>
                onChange({ ...action, status: Number(v) as RedirectStatus })
              }
            >
              <SelectTrigger id="redirect-status">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {REDIRECT_STATUSES.map((s) => (
                  <SelectItem key={s.value} value={String(s.value)}>
                    {s.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
        </div>
      )
    case 'challenge':
      return (
        <p className="text-sm text-muted-foreground">
          Visitors see a short proof-of-work page that their browser solves
          automatically. Scripts without a browser cannot pass it. After
          passing, the visitor continues to the rules below this one.
        </p>
      )
    case 'allow':
      return (
        <p className="text-sm text-muted-foreground">
          Place Allow rules above broader Block or Challenge rules to make
          exceptions, for example your office network above a rule that blocks
          /admin.
        </p>
      )
    case 'log':
      return (
        <p className="text-sm text-muted-foreground">
          Matching requests are tagged with this rule in the proxy logs and
          checking continues with the next rule.
        </p>
      )
  }
}
