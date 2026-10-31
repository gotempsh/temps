// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ProjectResponse } from '@/api/client'
import { PageHeader } from '@/components/layout/PageContainer'
import { Button } from '@/components/ui/button'
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from '@/components/ui/collapsible'
import { EmptyState } from '@/components/ui/empty-state'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { ReadFailure } from '@/components/ui/read-failure'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import { Textarea } from '@/components/ui/textarea'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { cn } from '@/lib/utils'
import { Button as BusyButton, Callout } from '@temps-sdk/ds'
import { useQuery } from '@tanstack/react-query'
import { ChevronRight, FileQuestion, FlaskConical } from 'lucide-react'
import { ActionIcon } from './action-icons'
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
  autoName,
  describeAction,
  emptyDraft,
  ruleWarnings,
  validateRule,
  withAutoName,
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

// Naming is optional: a blank name becomes a description of the rule.
const resolver: Resolver<EdgeRuleDraft> = async (raw) => {
  const values = withAutoName(raw)
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

/** Whether a rule uses anything that lives behind "More options". */
function usesAdvanced(draft: EdgeRuleDraft): boolean {
  return (
    draft.environmentId !== null ||
    (draft.action.type === 'block' && draft.action.body.trim() !== '') ||
    draft.expression.combinator === 'none' ||
    draft.expression.children.some((c) => c.kind === 'group')
  )
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
  const defaults = useMemo(
    () => (existing ? toDraft(existing) : emptyDraft()),
    [existing]
  )
  const [advanced, setAdvanced] = useState(() => usesAdvanced(defaults))
  const {
    control,
    register,
    handleSubmit,
    setValue,
    formState: { errors, submitCount, isSubmitting },
  } = useForm<EdgeRuleDraft>({ defaultValues: defaults, resolver })
  // useWatch, unlike watch(), returns a stable value React can memoize on.
  const values = useWatch({ control }) as EdgeRuleDraft
  const issues = useMemo(() => validateRule(withAutoName(values)), [values])
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
  const suggestedName = autoName(values)

  // The tester sees the saved list with this draft in its place, so a rule
  // can be tried before it is saved.
  const testRules = useMemo(() => {
    const draftRule: EdgeRule = {
      ...withAutoName(values),
      id: existing?.id ?? DRAFT_ID,
      revision: existing?.revision ?? 0,
      createdAt: existing?.createdAt ?? '',
      updatedAt: existing?.updatedAt ?? '',
    }
    return existing
      ? rules.map((r) => (r.id === existing.id ? draftRule : r))
      : [...rules, draftRule]
  }, [existing, rules, values])

  const onSubmit = handleSubmit(async (draft) => {
    let saved: EdgeRule
    try {
      saved = existing
        ? await updateRule.mutateAsync({
            ruleId: existing.id,
            revision: existing.revision,
            patch: draft,
          })
        : await createRule.mutateAsync(draft)
    } catch {
      // The mutation's error toast explains it (a 409 says the rule changed
      // elsewhere); the form keeps the user's edits.
      return
    }
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
  const position = existing
    ? rules.findIndex((r) => r.id === existing.id) + 1
    : rules.length + 1

  return (
    <form
      onSubmit={(e) => void onSubmit(e).catch(() => undefined)}
      className="w-full min-w-0 space-y-6"
      noValidate
    >
      <PageHeader
        title={existing ? existing.name : 'New rule'}
        description={`Rule ${position} of ${existing ? rules.length : rules.length + 1}, checked top to bottom.`}
        actions={
          <Button
            type="button"
            variant="outline"
            onClick={() => setTestOpen(true)}
          >
            <FlaskConical />
            Test
          </Button>
        }
      />

      <div className="divide-y rounded-lg border bg-card text-card-foreground">
        <section className="space-y-1.5 p-4 sm:px-6">
          <Label htmlFor="rule-name">
            Name{' '}
            <span className="font-normal text-muted-foreground">
              (optional)
            </span>
          </Label>
          <Input
            id="rule-name"
            maxLength={LIMITS.nameLength}
            placeholder={suggestedName}
            aria-invalid={!!errors.name}
            aria-describedby={errors.name ? 'rule-name-error' : undefined}
            {...register('name')}
          />
          {errors.name && (
            <p id="rule-name-error" className="text-xs text-destructive">
              {errors.name.message}
            </p>
          )}
        </section>

        <section
          aria-labelledby="rule-when"
          className="grid gap-3 p-4 sm:grid-cols-[64px_1fr] sm:px-6"
        >
          <h2
            id="rule-when"
            className="pt-2 text-sm font-semibold uppercase tracking-wide text-muted-foreground"
          >
            If
          </h2>
          <Controller
            control={control}
            name="expression"
            render={({ field }) => (
              <ConditionBuilder
                value={field.value}
                onChange={field.onChange}
                issues={nodeIssues}
                advanced={advanced}
              />
            )}
          />
        </section>

        <section
          aria-labelledby="rule-then"
          className="grid gap-3 p-4 sm:grid-cols-[64px_1fr] sm:px-6"
        >
          <h2
            id="rule-then"
            className="pt-2 text-sm font-semibold uppercase tracking-wide text-muted-foreground"
          >
            Then
          </h2>
          <div className="space-y-2">
            <div className="flex flex-col gap-2 sm:flex-row sm:items-start">
              <ActionPicker
                value={action.type}
                onChange={(type) =>
                  type !== action.type &&
                  setValue('action', defaultAction(type), { shouldDirty: true })
                }
              />
              <ActionFields
                action={action}
                onChange={(next) =>
                  setValue('action', next, { shouldDirty: true })
                }
                invalid={!!actionError}
              />
            </div>
            <p className="text-xs text-muted-foreground">
              {ACTIONS.find((a) => a.type === action.type)?.description}
              {action.type === 'redirect' && (
                <>
                  {' '}
                  <code className="font-mono">{'{path}'}</code> and{' '}
                  <code className="font-mono">{'{query}'}</code> insert the
                  visitor&apos;s path and query string.
                </>
              )}
            </p>
            {actionError && (
              <p className="text-xs text-destructive">{actionError.message}</p>
            )}
          </div>
        </section>

        <Collapsible open={advanced} onOpenChange={setAdvanced}>
          <CollapsibleTrigger asChild>
            <button
              type="button"
              className="flex w-full items-center gap-2 px-4 py-3 text-left text-sm text-muted-foreground hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring sm:px-6"
            >
              <ChevronRight
                className={cn(
                  'size-4 transition-transform',
                  advanced && 'rotate-90'
                )}
                aria-hidden="true"
              />
              More options
              <span className="text-xs">
                environment, condition groups
                {action.type === 'block' ? ', response body' : ''}
              </span>
            </button>
          </CollapsibleTrigger>
          <CollapsibleContent className="grid grid-cols-1 gap-4 px-4 pb-4 sm:px-6 md:grid-cols-2">
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
            {action.type === 'block' && (
              <div className="space-y-1.5">
                <Label htmlFor="block-body">Response body</Label>
                <Textarea
                  id="block-body"
                  rows={2}
                  maxLength={LIMITS.blockBodyBytes}
                  value={action.body}
                  placeholder="Forbidden"
                  onChange={(e) =>
                    setValue(
                      'action',
                      { ...action, body: e.target.value },
                      { shouldDirty: true }
                    )
                  }
                />
                <p className="text-xs text-muted-foreground">
                  Plain text, up to {LIMITS.blockBodyBytes} bytes.
                </p>
              </div>
            )}
            <p className="text-xs text-muted-foreground md:col-span-2">
              Use <span className="font-medium">Add group</span> under the
              conditions to mix and/or, or to match when none of a set is true,
              for example a path that is not requested from your office network.
            </p>
          </CollapsibleContent>
        </Collapsible>
      </div>

      {warnings.length > 0 && (
        <Callout tone="warning" title="Check before enforcing">
          <ul className="list-disc space-y-1 pl-4">
            {warnings.map((w) => (
              <li key={w}>{w}</li>
            ))}
          </ul>
        </Callout>
      )}

      {submitCount > 0 && issues.length > 0 && (
        <p role="alert" className="text-sm text-destructive">
          Fix the highlighted {issues.length === 1 ? 'field' : 'fields'} to save
          the rule.
        </p>
      )}

      <div className="flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between">
        <Controller
          control={control}
          name="mode"
          render={({ field }) => (
            <div className="flex items-start gap-3">
              <Switch
                id="rule-enforce"
                checked={field.value === 'enforce'}
                onCheckedChange={(on) =>
                  field.onChange(on ? 'enforce' : 'log_only')
                }
                className="mt-0.5"
              />
              <div>
                <Label htmlFor="rule-enforce">Enforce</Label>
                <p className="text-xs text-muted-foreground">
                  {field.value === 'enforce'
                    ? `${describeAction(action)} on matching requests.`
                    : 'Off: matches are only logged, nothing is blocked.'}
                </p>
              </div>
            </div>
          )}
        />
        <div className="flex gap-2 sm:justify-end">
          <Button asChild variant="outline">
            <Link to={base}>Cancel</Link>
          </Button>
          <BusyButton type="submit" busy={isSubmitting} busyLabel="Saving…">
            {existing ? 'Save rule' : 'Create rule'}
          </BusyButton>
        </div>
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

function ActionPicker({
  value,
  onChange,
}: {
  value: ActionType
  onChange: (next: ActionType) => void
}) {
  const label = ACTIONS.find((a) => a.type === value)?.label
  return (
    <>
      <Label htmlFor="rule-action" className="sr-only">
        Action
      </Label>
      <Select value={value} onValueChange={(v) => onChange(v as ActionType)}>
        <SelectTrigger id="rule-action" className="w-full sm:w-[180px]">
          <SelectValue>
            <span className="flex items-center gap-2">
              <ActionIcon type={value} />
              {label}
            </span>
          </SelectValue>
        </SelectTrigger>
        <SelectContent className="w-[320px]">
          {ACTIONS.map((meta) => (
            <SelectItem key={meta.type} value={meta.type} className="py-2">
              <span className="flex items-start gap-2">
                <ActionIcon type={meta.type} className="mt-0.5" />
                <span>
                  <span className="block font-medium">{meta.label}</span>
                  <span className="block text-xs text-muted-foreground">
                    {meta.description}
                  </span>
                </span>
              </span>
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
    </>
  )
}

/** The action's own settings, on the same line as the action. */
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
        <>
          <Label htmlFor="block-status" className="sr-only">
            Response status
          </Label>
          <Select
            value={String(action.status)}
            onValueChange={(v) => onChange({ ...action, status: Number(v) })}
          >
            <SelectTrigger id="block-status" className="w-full sm:w-[240px]">
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
        </>
      )
    case 'redirect':
      return (
        <>
          <Label htmlFor="redirect-target" className="sr-only">
            Redirect to
          </Label>
          <Input
            id="redirect-target"
            value={action.target}
            placeholder="https://docs.example.com{path}"
            className="min-w-0 flex-1 font-mono"
            aria-invalid={invalid}
            onChange={(e) => onChange({ ...action, target: e.target.value })}
          />
          <Label htmlFor="redirect-status" className="sr-only">
            Redirect status
          </Label>
          <Select
            value={String(action.status)}
            onValueChange={(v) =>
              onChange({ ...action, status: Number(v) as RedirectStatus })
            }
          >
            <SelectTrigger id="redirect-status" className="w-full sm:w-[200px]">
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
        </>
      )
    case 'allow':
    case 'challenge':
    case 'log':
      return null
  }
}
