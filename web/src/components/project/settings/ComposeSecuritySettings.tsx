// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  updateComposeSecurity,
  type ComposeSecurityCheckDefinition,
  type UpdateComposeSecurityData,
} from '@/api/client'
import { getComposeSecurityOptions } from '@/api/client/@tanstack/react-query.gen'
import {
  AlertDialog,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogCancel,
} from '@/components/ui/alert-dialog'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from '@/components/ui/collapsible'
import { Input } from '@/components/ui/input'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { ChevronDown, ShieldAlert } from 'lucide-react'
import { useState } from 'react'
import { toast } from 'sonner'

export function ComposeSecuritySettings({ projectId }: { projectId: number }) {
  const [legacyAcknowledged, setLegacyAcknowledged] = useState(false)
  const [open, setOpen] = useState(false)
  const [search, setSearch] = useState('')
  const [pending, setPending] = useState<ComposeSecurityCheckDefinition | null>(
    null
  )
  const [acknowledged, setAcknowledged] = useState(false)
  const queryClient = useQueryClient()
  const options = getComposeSecurityOptions({ path: { id: projectId } })
  const query = useQuery(options)
  const mutation = useMutation({
    retry: false,
    mutationFn: async (request: {
      path: UpdateComposeSecurityData['path']
      body: UpdateComposeSecurityData['body']
    }) => {
      // Problem Details responses omit their status field; preserve the HTTP status.
      const result = await updateComposeSecurity({
        ...request,
        throwOnError: false,
        responseStyle: 'fields',
      })
      if (!result.data) {
        throw Object.assign(
          new Error('Could not save Compose security settings'),
          { status: result.response?.status }
        )
      }
      return result.data
    },
    onSuccess: (data) => {
      queryClient.setQueryData(options.queryKey, data)
      setPending(null)
      setAcknowledged(false)
      toast.success(
        'Compose security settings saved. Applies on the next deployment.'
      )
    },
    onError: (error) => {
      setPending(null)
      setAcknowledged(false)
      setLegacyAcknowledged(false)
      if ((error as { status?: number })?.status === 409) {
        toast.error(
          'Another administrator changed these settings. Review the refreshed policy and try again.'
        )
        void query.refetch()
      } else {
        toast.error(
          'Could not save Compose security settings. Refresh and try again.'
        )
      }
    },
  })
  const canEdit = query.data?.can_edit ?? false
  const disabled = query.data?.policy.disabled_checks ?? []
  const checks = query.data?.checks ?? []
  const searchText = search.trim().toLowerCase()
  const filtered = checks.filter((check) =>
    `${check.group} ${check.label} ${check.consequence}`
      .toLowerCase()
      .includes(searchText)
  )
  const groups = [...new Set(filtered.map((check) => check.group))]
  const saveCheck = (
    check: ComposeSecurityCheckDefinition,
    enforce: boolean
  ) => {
    if (!query.data || !query.data.can_edit) return
    const next = enforce
      ? disabled.filter((id) => id !== check.id)
      : [...new Set([...disabled, check.id])]
    mutation.mutate({
      path: { id: projectId },
      body: {
        policy: { disabled_checks: next },
        expected_policy: query.data.policy,
        acknowledge_risks: !enforce && acknowledged,
      },
    })
  }

  return (
    <Collapsible
      open={open}
      onOpenChange={setOpen}
      className="rounded-lg border"
      id="compose-security"
    >
      <CollapsibleTrigger asChild>
        <button
          type="button"
          className="flex w-full items-start gap-3 p-4 text-left"
        >
          <ShieldAlert className="mt-0.5 h-4 w-4 shrink-0 text-muted-foreground" />
          <span className="min-w-0 flex-1 space-y-1">
            <span className="block text-sm font-medium">
              Advanced security settings
            </span>
            <span className="block text-xs text-muted-foreground">
              For experienced administrators running trusted Docker Compose
              stacks. Disable individual checks only when you understand the
              access they allow.
            </span>
          </span>
          {disabled.length > 0 && (
            <Badge variant="destructive">{disabled.length} disabled</Badge>
          )}
          <ChevronDown
            className={`mt-0.5 h-4 w-4 shrink-0 transition-transform ${open ? 'rotate-180' : ''}`}
          />
        </button>
      </CollapsibleTrigger>
      {query.data?.legacy_migration_pending && (
        <p
          role="note"
          className="px-4 pb-4 text-sm text-amber-700 dark:text-amber-400"
        >
          This project has legacy “Disable sandbox” settings. They no longer
          apply to new deployments. Review and acknowledge the individual
          exceptions you need here before redeploying.
        </p>
      )}
      <CollapsibleContent className="space-y-4 border-t p-4">
        <p className="text-sm text-muted-foreground">
          Checks are enabled by default. Exceptions apply to this project on its
          next deployment and may expose the host or other projects. Docker
          configuration validity and protection of Temps-generated files always
          remain enforced. Ordinary named volumes need no exception. These
          settings replace the former per-service “Disable sandbox” option;
          configure any required exceptions here before your next deployment.
        </p>
        {query.isPending && (
          <div className="space-y-2">
            <Skeleton className="h-9 w-full" />
            <Skeleton className="h-24 w-full" />
          </div>
        )}
        {query.isError && (
          <div
            role="alert"
            className="flex flex-wrap items-center gap-2 text-sm"
          >
            Could not load Compose security settings.
            <Button
              variant="outline"
              size="sm"
              onClick={() => void query.refetch()}
            >
              Retry
            </Button>
          </div>
        )}
        {query.data && (
          <>
            {!query.data.can_edit && (
              <p className="text-sm text-muted-foreground">
                Only instance administrators can change these settings.
              </p>
            )}
            {query.data.legacy_migration_pending && canEdit && (
              <div className="space-y-2 rounded-md border p-3">
                <label className="flex items-start gap-2 text-sm">
                  <Checkbox
                    checked={legacyAcknowledged}
                    onCheckedChange={(checked) =>
                      setLegacyAcknowledged(checked === true)
                    }
                    disabled={mutation.isPending}
                  />
                  I have reviewed the exceptions this project needs for its next
                  deployment.
                </label>
                <Button
                  size="sm"
                  variant="outline"
                  disabled={!legacyAcknowledged || mutation.isPending}
                  onClick={() => {
                    if (!query.data) return
                    mutation.mutate({
                      path: { id: projectId },
                      body: {
                        policy: query.data.policy,
                        expected_policy: query.data.policy,
                        acknowledge_risks: false,
                        acknowledge_legacy_migration: true,
                      },
                    })
                  }}
                >
                  Complete legacy migration
                </Button>
              </div>
            )}
            <Input
              aria-label="Search Compose security checks"
              placeholder="Search security checks…"
              value={search}
              onChange={(event) => setSearch(event.target.value)}
            />
            {filtered.length === 0 && (
              <p className="text-sm text-muted-foreground">
                No security checks match your search.
              </p>
            )}
            {groups.map((group) => (
              <section key={group} aria-label={group} className="space-y-2">
                <h4 className="text-sm font-medium">{group}</h4>
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>Security check</TableHead>
                      <TableHead className="w-28 text-right">
                        Enforced
                      </TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {filtered
                      .filter((check) => check.group === group)
                      .map((check) => {
                        const enforced = !disabled.includes(check.id)
                        return (
                          <TableRow key={check.id}>
                            <TableCell className="whitespace-normal">
                              <label
                                htmlFor={`compose-check-${check.id}`}
                                className="text-sm font-medium"
                              >
                                {check.label}
                              </label>
                              <p className="mt-1 text-xs text-muted-foreground">
                                When disabled: {check.consequence}
                              </p>
                            </TableCell>
                            <TableCell className="text-right">
                              <Switch
                                id={`compose-check-${check.id}`}
                                checked={enforced}
                                disabled={!canEdit || mutation.isPending}
                                onCheckedChange={(checked) => {
                                  if (checked) saveCheck(check, true)
                                  else {
                                    setAcknowledged(false)
                                    setPending(check)
                                  }
                                }}
                              />
                              <span
                                className={`mt-1 block text-xs ${enforced ? 'text-muted-foreground' : 'text-destructive'}`}
                              >
                                {enforced ? 'Enabled' : 'Disabled'}
                              </span>
                            </TableCell>
                          </TableRow>
                        )
                      })}
                  </TableBody>
                </Table>
              </section>
            ))}
          </>
        )}
      </CollapsibleContent>
      <AlertDialog
        open={pending !== null}
        onOpenChange={(value) => {
          if (!value && !mutation.isPending) {
            setPending(null)
            setAcknowledged(false)
          }
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Disable this security check?</AlertDialogTitle>
            <AlertDialogDescription>
              {pending?.label}: {pending?.consequence} This applies to every
              service in this project on the next deployment. Other checks
              remain enforced.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <div className="flex items-start gap-2">
            <Checkbox
              id="acknowledge-compose-security"
              checked={acknowledged}
              onCheckedChange={(value) => setAcknowledged(value === true)}
              disabled={mutation.isPending}
            />
            <label htmlFor="acknowledge-compose-security" className="text-sm">
              I trust this stack and understand that this exception can affect
              the host and other projects.
            </label>
          </div>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={mutation.isPending}>
              Keep enabled
            </AlertDialogCancel>
            <Button
              variant="destructive"
              disabled={!acknowledged || mutation.isPending}
              onClick={() => {
                if (pending) saveCheck(pending, false)
              }}
            >
              {mutation.isPending ? 'Saving…' : 'Disable check'}
            </Button>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </Collapsible>
  )
}
