// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  createProjectShieldEntryMutation,
  deleteProjectShieldEntryMutation,
  listProjectShieldEntriesOptions,
  listProjectShieldEntriesQueryKey,
  updateProjectShieldEntryMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  ShieldEntryResponse,
  ShieldEntryVerdict,
} from '@/api/client/types.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
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
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { formatLocalDateTime } from '@/lib/date'
import {
  isoToLocalInput,
  localInputToIso,
  shieldVerdictBadge,
} from '@/lib/shield'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { ListChecks, Loader2, Pencil, Plus, Trash2 } from 'lucide-react'
import { useState } from 'react'
import { Controller, useForm, useWatch } from 'react-hook-form'
import { toast } from 'sonner'
import { z } from 'zod'
import { ShieldSection } from './ShieldSection'

const VERDICTS: ReadonlyArray<{
  value: ShieldEntryVerdict
  label: string
  description: string
}> = [
  {
    value: 'allow',
    label: 'Allow',
    description: 'Always passes, whatever else matches.',
  },
  { value: 'log', label: 'Log', description: 'Recorded, never acted on.' },
  {
    value: 'challenge',
    label: 'Challenge',
    description: 'Challenged in selective and attack mode.',
  },
  {
    value: 'block',
    label: 'Block',
    description: 'Refused with a 403 in selective and attack mode.',
  },
]

const entrySchema = z
  .object({
    match: z.enum(['network', 'ja4']),
    value: z.string().trim().max(128, 'At most 128 characters.'),
    verdict: z.enum(['allow', 'log', 'challenge', 'block']),
    note: z.string().trim().max(500, 'At most 500 characters.'),
    expires_at: z.string(),
  })
  .superRefine((entry, ctx) => {
    if (entry.value === '') {
      ctx.addIssue({
        code: 'custom',
        path: ['value'],
        message:
          entry.match === 'network'
            ? 'Enter an address or a CIDR range.'
            : 'Enter a JA4 fingerprint.',
      })
    }
    if (entry.expires_at !== '') {
      const expiry = localInputToIso(entry.expires_at)
      if (expiry === null || new Date(expiry).getTime() <= Date.now()) {
        ctx.addIssue({
          code: 'custom',
          path: ['expires_at'],
          message: 'Choose a time in the future, or leave it empty.',
        })
      }
    }
  })

type EntryForm = z.infer<typeof entrySchema>

const EMPTY_ENTRY: EntryForm = {
  match: 'network',
  value: '',
  verdict: 'block',
  note: '',
  expires_at: '',
}

function entryToForm(entry: ShieldEntryResponse): EntryForm {
  return {
    match: entry.ja4 ? 'ja4' : 'network',
    value: entry.network ?? entry.ja4 ?? '',
    verdict: entry.verdict,
    note: entry.note ?? '',
    expires_at: isoToLocalInput(entry.expires_at),
  }
}

function entryLabel(entry: ShieldEntryResponse) {
  return entry.network ?? entry.ja4 ?? `entry ${entry.id}`
}

export function ShieldEntriesPanel({ projectId }: { projectId: number }) {
  const queryClient = useQueryClient()
  const path = { project_id: projectId }
  const [dialogOpen, setDialogOpen] = useState(false)
  // The entry being edited; `null` while creating one.
  const [editing, setEditing] = useState<ShieldEntryResponse | null>(null)
  const [deleteTarget, setDeleteTarget] = useState<ShieldEntryResponse | null>(
    null
  )

  const entries = useQuery({ ...listProjectShieldEntriesOptions({ path }) })
  const invalidate = () =>
    queryClient.invalidateQueries({
      queryKey: listProjectShieldEntriesQueryKey({ path }),
    })

  const form = useForm<EntryForm>({
    resolver: zodResolver(entrySchema),
    defaultValues: EMPTY_ENTRY,
  })
  const errors = form.formState.errors
  const match = useWatch({ control: form.control, name: 'match' })
  const chosenVerdict = useWatch({ control: form.control, name: 'verdict' })

  const onSaved = (message: string) => {
    toast.success(message)
    setDialogOpen(false)
    setEditing(null)
    void invalidate()
  }

  const create = useMutation({
    ...createProjectShieldEntryMutation(),
    meta: { errorTitle: 'Failed to add Shield entry' },
    onSuccess: () => onSaved('Shield entry added'),
  })
  const update = useMutation({
    ...updateProjectShieldEntryMutation(),
    meta: { errorTitle: 'Failed to update Shield entry' },
    onSuccess: () => onSaved('Shield entry updated'),
  })
  const remove = useMutation({
    ...deleteProjectShieldEntryMutation(),
    meta: { errorTitle: 'Failed to delete Shield entry' },
    onSuccess: () => {
      toast.success('Shield entry deleted')
      setDeleteTarget(null)
      void invalidate()
    },
  })
  const saving = create.isPending || update.isPending

  const openCreate = () => {
    setEditing(null)
    form.reset(EMPTY_ENTRY)
    setDialogOpen(true)
  }
  const openEdit = (entry: ShieldEntryResponse) => {
    setEditing(entry)
    form.reset(entryToForm(entry))
    setDialogOpen(true)
  }

  const submit = form.handleSubmit((values) => {
    const note = values.note === '' ? null : values.note
    const expires_at = localInputToIso(values.expires_at)
    if (editing) {
      update.mutate({
        path: { ...path, entry_id: editing.id },
        body: { verdict: values.verdict, note, expires_at },
      })
      return
    }
    create.mutate({
      path,
      body: {
        network: values.match === 'network' ? values.value : null,
        ja4: values.match === 'ja4' ? values.value : null,
        verdict: values.verdict,
        note,
        expires_at,
      },
    })
  })

  const rows = entries.data ?? []
  const addButton = (
    <Button type="button" size="sm" onClick={openCreate}>
      <Plus className="mr-2 h-4 w-4" />
      Add entry
    </Button>
  )

  return (
    <ShieldSection
      id="shield-entries"
      title="Operator entries"
      description="Addresses, ranges and fingerprints with a verdict you set. They apply to every environment of this service."
      actions={rows.length > 0 ? addButton : undefined}
    >
      {entries.error && (
        <div className="p-4">
          <ReadFailure
            resource="Shield entries"
            error={entries.error}
            cached={entries.data !== undefined}
            onRetry={() => entries.refetch()}
            retrying={entries.isFetching}
            embedded
          />
        </div>
      )}
      {entries.data === undefined ? (
        entries.error ? null : (
          <div
            role="status"
            aria-label="Loading Shield entries"
            className="space-y-2 p-4"
          >
            <Skeleton className="h-10 w-full" />
            <Skeleton className="h-10 w-full" />
          </div>
        )
      ) : rows.length === 0 ? (
        <EmptyState
          size="compact"
          icon={ListChecks}
          title="No operator entries yet"
          description="Allow a partner's range, or block or challenge an address the lists do not know about."
          action={addButton}
        />
      ) : (
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>Matches</TableHead>
              <TableHead>Verdict</TableHead>
              <TableHead className="hidden md:table-cell">Note</TableHead>
              <TableHead className="hidden md:table-cell">Expires</TableHead>
              <TableHead className="text-right">Actions</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows.map((entry) => (
              <TableRow key={entry.id}>
                <TableCell>
                  <span className="font-mono text-xs break-all">
                    {entryLabel(entry)}
                  </span>
                  <span className="block text-xs text-muted-foreground">
                    {entry.ja4 ? 'JA4 fingerprint' : 'Address range'}
                  </span>
                </TableCell>
                <TableCell>
                  <Badge variant={shieldVerdictBadge(entry.verdict)}>
                    {entry.verdict}
                  </Badge>
                </TableCell>
                <TableCell className="hidden max-w-[20rem] truncate text-sm text-muted-foreground md:table-cell">
                  {entry.note || '—'}
                </TableCell>
                <TableCell className="hidden text-sm text-muted-foreground md:table-cell">
                  {entry.expires_at
                    ? formatLocalDateTime(entry.expires_at)
                    : 'Never'}
                </TableCell>
                <TableCell className="text-right">
                  <div className="flex justify-end gap-2">
                    <Button
                      type="button"
                      size="sm"
                      variant="outline"
                      aria-label={`Edit ${entryLabel(entry)}`}
                      onClick={() => openEdit(entry)}
                    >
                      <Pencil className="h-4 w-4" />
                      <span className="ml-1 max-sm:sr-only">Edit</span>
                    </Button>
                    <Button
                      type="button"
                      size="sm"
                      variant="outline"
                      aria-label={`Delete ${entryLabel(entry)}`}
                      onClick={() => setDeleteTarget(entry)}
                    >
                      <Trash2 className="h-4 w-4" />
                      <span className="ml-1 max-sm:sr-only">Delete</span>
                    </Button>
                  </div>
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}

      <Dialog
        open={dialogOpen}
        onOpenChange={(open) => {
          if (saving) return
          setDialogOpen(open)
          if (!open) setEditing(null)
        }}
      >
        <DialogContent className="sm:max-w-lg">
          <DialogHeader>
            <DialogTitle>
              {editing ? 'Edit Shield entry' : 'Add Shield entry'}
            </DialogTitle>
            <DialogDescription>
              {editing
                ? 'Change the verdict, note or expiry. What the entry matches stays the same.'
                : 'The entry applies to every environment of this service, usually within seconds.'}
            </DialogDescription>
          </DialogHeader>
          <form noValidate onSubmit={submit} className="space-y-4">
            <div className="grid gap-4 sm:grid-cols-[10rem_minmax(0,1fr)]">
              <div className="space-y-2">
                <Label htmlFor="shield-entry-match">Match</Label>
                <Controller
                  control={form.control}
                  name="match"
                  render={({ field }) => (
                    <Select
                      value={field.value}
                      onValueChange={field.onChange}
                      disabled={editing !== null}
                    >
                      <SelectTrigger id="shield-entry-match">
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        <SelectItem value="network">Address range</SelectItem>
                        <SelectItem value="ja4">JA4 fingerprint</SelectItem>
                      </SelectContent>
                    </Select>
                  )}
                />
              </div>
              <div className="min-w-0 space-y-2">
                <Label htmlFor="shield-entry-value">
                  {match === 'ja4' ? 'Fingerprint' : 'Address or CIDR'}
                </Label>
                <Input
                  id="shield-entry-value"
                  className="font-mono"
                  autoComplete="off"
                  spellCheck={false}
                  placeholder={
                    match === 'ja4'
                      ? 't13d1516h2_8daaf6152771_02713d6af862'
                      : '198.51.100.0/24'
                  }
                  readOnly={editing !== null}
                  aria-invalid={errors.value ? 'true' : undefined}
                  aria-describedby={
                    errors.value ? 'shield-entry-value-error' : undefined
                  }
                  {...form.register('value')}
                />
                {errors.value && (
                  <p
                    id="shield-entry-value-error"
                    className="text-sm text-destructive"
                  >
                    {errors.value.message}
                  </p>
                )}
              </div>
            </div>
            <div className="space-y-2">
              <Label htmlFor="shield-entry-verdict">Verdict</Label>
              <Controller
                control={form.control}
                name="verdict"
                render={({ field }) => (
                  <Select value={field.value} onValueChange={field.onChange}>
                    <SelectTrigger id="shield-entry-verdict">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      {VERDICTS.map((verdict) => (
                        <SelectItem key={verdict.value} value={verdict.value}>
                          {verdict.label}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                )}
              />
              <p className="text-xs text-muted-foreground">
                {
                  VERDICTS.find((verdict) => verdict.value === chosenVerdict)
                    ?.description
                }
              </p>
            </div>
            <div className="space-y-2">
              <Label htmlFor="shield-entry-note">Note (optional)</Label>
              <Input
                id="shield-entry-note"
                placeholder="Why this entry exists"
                aria-invalid={errors.note ? 'true' : undefined}
                {...form.register('note')}
              />
              {errors.note && (
                <p className="text-sm text-destructive">
                  {errors.note.message}
                </p>
              )}
            </div>
            <div className="space-y-2">
              <Label htmlFor="shield-entry-expires">Expires (optional)</Label>
              <Input
                id="shield-entry-expires"
                type="datetime-local"
                aria-invalid={errors.expires_at ? 'true' : undefined}
                aria-describedby={
                  errors.expires_at ? 'shield-entry-expires-error' : undefined
                }
                {...form.register('expires_at')}
              />
              {errors.expires_at && (
                <p
                  id="shield-entry-expires-error"
                  className="text-sm text-destructive"
                >
                  {errors.expires_at.message}
                </p>
              )}
            </div>
            <DialogFooter>
              <Button
                type="button"
                variant="outline"
                disabled={saving}
                onClick={() => {
                  setDialogOpen(false)
                  setEditing(null)
                }}
              >
                Cancel
              </Button>
              <Button type="submit" disabled={saving}>
                {saving && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
                {editing ? 'Save entry' : 'Add entry'}
              </Button>
            </DialogFooter>
          </form>
        </DialogContent>
      </Dialog>

      <ConfirmDeleteDialog
        open={deleteTarget !== null}
        onOpenChange={(open) => {
          if (!open) setDeleteTarget(null)
        }}
        resourceType="Shield entry"
        resourceName={deleteTarget ? entryLabel(deleteTarget) : ''}
        consequences="Requests it matched are judged by the lists alone again."
        isPending={remove.isPending}
        onConfirm={() => {
          if (!deleteTarget) return
          remove.mutate({ path: { ...path, entry_id: deleteTarget.id } })
        }}
      />
    </ShieldSection>
  )
}
