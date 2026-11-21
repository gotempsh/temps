// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useState } from 'react'
import { zodResolver } from '@hookform/resolvers/zod'
import {
  Controller,
  useFieldArray,
  useForm,
  type Control,
} from 'react-hook-form'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { ArrowDown, ArrowUp, Plus, Trash2, UserRound } from 'lucide-react'
import { toast } from 'sonner'
import {
  listErrorOwnershipRulesOptions,
  listErrorOwnershipRulesQueryKey,
  replaceErrorOwnershipRulesMutation,
  searchErrorGroupAssigneesOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type { OwnershipRuleView } from '@/api/client/types.gen'
import { PageHeader } from '@/components/layout/PageContainer'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Skeleton } from '@/components/ui/skeleton'
import { readFailureServerDetail } from '@/lib/read-failure'
import {
  editedSinceSave,
  MAX_OWNERSHIP_RULES,
  ownershipRulesSchema,
  rulesFormReset,
  rulesToForm,
  type OwnershipRulesFormData,
} from './source-control'

function OwnerPicker({
  projectId,
  value,
  label,
  invalid,
  onChange,
}: {
  projectId: number
  value: number
  label: string
  invalid: boolean
  onChange: (id: number, label: string) => void
}) {
  const [open, setOpen] = useState(false)
  const [search, setSearch] = useState('')
  const candidates = useQuery({
    ...searchErrorGroupAssigneesOptions({
      path: { project_id: projectId },
      query: { q: search.trim() },
    }),
    enabled: open,
    staleTime: 30_000,
  })
  const users = candidates.data?.users ?? []
  return (
    <DropdownMenu open={open} onOpenChange={setOpen}>
      <DropdownMenuTrigger asChild>
        <Button
          type="button"
          variant="outline"
          className="w-full justify-start sm:w-56"
          aria-invalid={invalid ? true : undefined}
        >
          <UserRound className="h-4 w-4" />
          <span className="truncate">
            {value > 0 ? label : 'Choose an owner'}
          </span>
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" className="w-72">
        <div className="p-2">
          <Input
            autoFocus
            placeholder="Search people"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            onKeyDown={(e) => e.stopPropagation()}
          />
        </div>
        <DropdownMenuLabel>People who can open this project</DropdownMenuLabel>
        {candidates.isPending ? (
          <p className="px-2 py-1.5 text-sm text-muted-foreground">Loading…</p>
        ) : users.length === 0 ? (
          <p className="px-2 py-1.5 text-sm text-muted-foreground">
            Nobody matches
          </p>
        ) : (
          users.map((user) => (
            <DropdownMenuItem
              key={user.id}
              onSelect={() => onChange(user.id, user.name)}
            >
              <span className="truncate">{user.name}</span>
              <span className="ml-auto truncate text-xs text-muted-foreground">
                {user.email}
              </span>
            </DropdownMenuItem>
          ))
        )}
      </DropdownMenuContent>
    </DropdownMenu>
  )
}

function RuleRow({
  index,
  count,
  projectId,
  control,
  onMove,
  onRemove,
}: {
  index: number
  count: number
  projectId: number
  control: Control<OwnershipRulesFormData>
  onMove: (from: number, to: number) => void
  onRemove: (index: number) => void
}) {
  return (
    <li className="flex flex-col gap-2 rounded-md border p-3 sm:flex-row sm:items-start">
      <span className="text-xs text-muted-foreground sm:mt-2.5 sm:w-6">
        {index + 1}.
      </span>
      <Controller
        control={control}
        name={`rules.${index}.pattern`}
        render={({ field, fieldState }) => (
          <div className="min-w-0 flex-1 space-y-1">
            <Label htmlFor={`rule-pattern-${index}`} className="sr-only">
              Path pattern {index + 1}
            </Label>
            <Input
              id={`rule-pattern-${index}`}
              placeholder="src/billing/"
              aria-invalid={fieldState.error ? true : undefined}
              {...field}
            />
            {fieldState.error && (
              <p className="text-xs text-destructive">
                {fieldState.error.message}
              </p>
            )}
          </div>
        )}
      />
      <Controller
        control={control}
        name={`rules.${index}.owner_user_id`}
        render={({ field, fieldState }) => (
          <Controller
            control={control}
            name={`rules.${index}.owner_label`}
            render={({ field: labelField }) => (
              <div className="space-y-1">
                <OwnerPicker
                  projectId={projectId}
                  value={field.value}
                  label={labelField.value}
                  invalid={!!fieldState.error}
                  onChange={(id, label) => {
                    field.onChange(id)
                    labelField.onChange(label)
                  }}
                />
                {fieldState.error && (
                  <p className="text-xs text-destructive">
                    {fieldState.error.message}
                  </p>
                )}
              </div>
            )}
          />
        )}
      />
      <div className="flex gap-1">
        <Button
          type="button"
          variant="ghost"
          size="sm"
          aria-label={`Move rule ${index + 1} up`}
          disabled={index === 0}
          onClick={() => onMove(index, index - 1)}
        >
          <ArrowUp className="h-4 w-4" />
        </Button>
        <Button
          type="button"
          variant="ghost"
          size="sm"
          aria-label={`Move rule ${index + 1} down`}
          disabled={index === count - 1}
          onClick={() => onMove(index, index + 1)}
        >
          <ArrowDown className="h-4 w-4" />
        </Button>
        <Button
          type="button"
          variant="ghost"
          size="sm"
          aria-label={`Remove rule ${index + 1}`}
          onClick={() => onRemove(index)}
        >
          <Trash2 className="h-4 w-4" />
        </Button>
      </div>
    </li>
  )
}

function RulesForm({
  projectId,
  rules,
}: {
  projectId: number
  rules: OwnershipRuleView[]
}) {
  const queryClient = useQueryClient()
  const form = useForm<OwnershipRulesFormData>({
    resolver: zodResolver(ownershipRulesSchema),
    defaultValues: rulesToForm(rules),
  })
  const { reset } = form
  const { isDirty } = form.formState
  useEffect(() => {
    const next = rulesFormReset(isDirty, rules)
    if (next) reset(next)
  }, [reset, rules, isDirty])
  const { fields, append, move, remove } = useFieldArray({
    control: form.control,
    name: 'rules',
  })
  const save = useMutation({
    ...replaceErrorOwnershipRulesMutation(),
    meta: { errorTitle: 'Failed to save the ownership rules' },
    onSuccess: (next, variables) => {
      toast.success('Ownership rules saved')
      // The saved rules are now the form's baseline. Without later edits
      // the form shows them and is clean again, so later changes from the
      // server show; edits made while the save ran are kept, still unsaved.
      reset(rulesToForm(next.data), {
        keepValues: editedSinceSave(form.getValues(), variables.body.rules),
      })
      queryClient.setQueryData(
        listErrorOwnershipRulesQueryKey({ path: { project_id: projectId } }),
        next
      )
    },
  })
  const listError = form.formState.errors.rules?.root?.message
  return (
    <form
      noValidate
      className="space-y-4"
      onSubmit={form.handleSubmit((data) =>
        save.mutate({
          path: { project_id: projectId },
          body: {
            rules: data.rules.map((rule) => ({
              pattern: rule.pattern.trim(),
              owner_user_id: rule.owner_user_id,
            })),
          },
        })
      )}
    >
      {fields.length === 0 ? (
        <p className="text-sm text-muted-foreground">
          No rules yet. Add one to assign new errors in{' '}
          <code className="text-xs">src/billing/</code> to the person who owns
          that code.
        </p>
      ) : (
        <ol className="space-y-2">
          {fields.map((field, index) => (
            <RuleRow
              key={field.id}
              index={index}
              count={fields.length}
              projectId={projectId}
              control={form.control}
              onMove={move}
              onRemove={remove}
            />
          ))}
        </ol>
      )}
      {listError && <p className="text-xs text-destructive">{listError}</p>}
      <div className="flex flex-wrap gap-2">
        <Button
          type="button"
          variant="outline"
          disabled={fields.length >= MAX_OWNERSHIP_RULES}
          onClick={() =>
            append({ pattern: '', owner_user_id: 0, owner_label: '' })
          }
        >
          <Plus className="h-4 w-4" />
          Add rule
        </Button>
        <Button type="submit" disabled={save.isPending}>
          {save.isPending ? 'Saving…' : 'Save rules'}
        </Button>
      </div>
    </form>
  )
}

/**
 * Ownership rules (ADR 111, D5): who owns the code under a path. A new
 * error group is assigned to the owner of the first rule matching one of its
 * frames, innermost first.
 */
export function OwnershipRulesSettings({ projectId }: { projectId: number }) {
  const rules = useQuery(
    listErrorOwnershipRulesOptions({ path: { project_id: projectId } })
  )
  return (
    <div className="space-y-4">
      <PageHeader title="Ownership" />
      <Card>
        <CardContent className="space-y-4 pt-6">
          <p className="text-sm text-muted-foreground">
            When an error first happens, its stack is matched against these
            rules in order, innermost frame first. The first match assigns the
            new group to the rule&apos;s owner. Groups already assigned are left
            alone. Patterns are repository paths: <code>src/billing/</code>{' '}
            covers a directory, <code>*.py</code> a file name anywhere, and{' '}
            <code>src/**/cart.ts</code> any depth.
          </p>
          {rules.data ? (
            <>
              {rules.isError && (
                <Alert variant="destructive">
                  <AlertDescription>
                    The ownership rules could not be refreshed; these are the
                    rules last loaded.
                  </AlertDescription>
                </Alert>
              )}
              <RulesForm projectId={projectId} rules={rules.data.data} />
            </>
          ) : rules.isError ? (
            <Alert variant="destructive">
              <AlertDescription>
                {readFailureServerDetail(rules.error) ??
                  'The ownership rules could not be loaded. Retry.'}
              </AlertDescription>
            </Alert>
          ) : (
            <div className="space-y-2">
              <Skeleton className="h-12 w-full" />
              <Skeleton className="h-12 w-full" />
            </div>
          )}
        </CardContent>
      </Card>
    </div>
  )
}
