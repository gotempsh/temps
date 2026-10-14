// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Per-project / per-environment IP allow or deny list.
 *
 * One list per scope. An environment list replaces the project-wide list for
 * that environment; an environment without its own list inherits the
 * project's. The same component serves both scopes so the two never drift.
 */
import {
  deleteEnvironmentIpAccess,
  deleteProjectIpAccess,
  getEnvironmentIpAccess,
  getProjectIpAccess,
  putEnvironmentIpAccess,
  putProjectIpAccess,
  testEnvironmentIpAccess,
  testProjectIpAccess,
} from '@/api/client'
import {
  getEnvironmentIpAccessQueryKey,
  getProjectIpAccessQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  ProjectIpAccessDetail,
  SetProjectIpAccessRequest,
  TestProjectIpAccessResponse,
} from '@/api/client/types.gen'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group'
import { Skeleton } from '@/components/ui/skeleton'
import { SettingsSection } from '@/components/ui/settings-section'
import { ErrorAlert } from '@/components/utils/ErrorAlert'
import { problemDetail } from '@/lib/api-problem'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  CircleAlert,
  Network,
  Plus,
  Search,
  Trash2,
  UserRound,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { Controller, useFieldArray, useForm, useWatch } from 'react-hook-form'
import { toast } from 'sonner'
import {
  MAX_CIDRS,
  MAX_DESCRIPTION_LENGTH,
  TAILSCALE_PRESETS,
  hasAllPresets,
  hostCidr,
  ipAccessFormSchema,
  modeLabel,
  policySummary,
  type IpAccessFormValues,
  type IpAccessMode,
} from './ip-access'

interface ProjectIpAccessCardProps {
  projectId: number
  /** Set to manage one environment's list instead of the project-wide one. */
  environmentId?: number
}

const MODES: { value: IpAccessMode; hint: string }[] = [
  { value: 'disabled', hint: 'Anyone can reach it. This is the default.' },
  {
    value: 'allow',
    hint: 'Everyone else gets a 403. Use for private or internal apps.',
  },
  { value: 'deny', hint: 'Listed addresses get a 403, everyone else is fine.' },
]

function isLockoutError(error: unknown): boolean {
  return (
    typeof error === 'object' &&
    error !== null &&
    (error as { status?: unknown }).status === 409
  )
}

export function ProjectIpAccessCard({
  projectId,
  environmentId,
}: ProjectIpAccessCardProps) {
  const queryClient = useQueryClient()
  const isEnvironment = environmentId !== undefined
  const scopeNoun = isEnvironment ? 'environment' : 'project'

  const queryKey = isEnvironment
    ? getEnvironmentIpAccessQueryKey({
        path: { project_id: projectId, environment_id: environmentId },
      })
    : getProjectIpAccessQueryKey({ path: { project_id: projectId } })

  const policyQuery = useQuery({
    queryKey,
    queryFn: async (): Promise<ProjectIpAccessDetail> => {
      const result = isEnvironment
        ? await getEnvironmentIpAccess({
            path: { project_id: projectId, environment_id: environmentId },
            throwOnError: true,
          })
        : await getProjectIpAccess({
            path: { project_id: projectId },
            throwOnError: true,
          })
      return result.data
    },
  })
  const policy = policyQuery.data

  const form = useForm<IpAccessFormValues>({
    resolver: zodResolver(ipAccessFormSchema),
    defaultValues: { mode: 'disabled', cidrs: [] },
  })
  const { fields, append, remove, replace } = useFieldArray({
    control: form.control,
    name: 'cidrs',
  })

  // Load the saved policy into the form whenever the server copy changes, but
  // never over edits the user has not saved yet.
  const savedAt = policy?.updated_at ?? null
  const hasPolicy = policy !== undefined
  useEffect(() => {
    if (!policy) return
    form.reset({
      mode: policy.mode,
      cidrs: policy.cidrs.map((entry) => ({
        cidr: entry.cidr,
        description: entry.description ?? '',
      })),
    })
    // `policy` identity changes on refetch; the timestamp and scope identify
    // a genuinely different saved version.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [hasPolicy, savedAt, projectId, environmentId])

  const mode = useWatch({ control: form.control, name: 'mode' })
  const currentCidrs = useWatch({ control: form.control, name: 'cidrs' })

  const [lockout, setLockout] = useState<{
    detail: string
    request: SetProjectIpAccessRequest
  } | null>(null)
  const [confirmRemove, setConfirmRemove] = useState(false)
  const [probe, setProbe] = useState('')
  const [probeResult, setProbeResult] =
    useState<TestProjectIpAccessResponse | null>(null)

  const save = useMutation({
    mutationFn: async (body: SetProjectIpAccessRequest) => {
      const result = isEnvironment
        ? await putEnvironmentIpAccess({
            path: { project_id: projectId, environment_id: environmentId },
            body,
            throwOnError: true,
          })
        : await putProjectIpAccess({
            path: { project_id: projectId },
            body,
            throwOnError: true,
          })
      return result.data
    },
    onSuccess: (saved) => {
      queryClient.setQueryData(queryKey, saved)
      setLockout(null)
      setProbeResult(null)
      toast.success(
        saved.mode === 'disabled'
          ? 'IP access list saved (not enforced)'
          : 'IP access list saved'
      )
    },
    onError: (error, body) => {
      if (isLockoutError(error)) {
        setLockout({
          detail: problemDetail(error, 'This change would lock you out.'),
          request: body,
        })
        return
      }
      toast.error(problemDetail(error, 'Failed to save the IP access list'))
    },
  })

  const remove_ = useMutation({
    mutationFn: async () => {
      if (isEnvironment) {
        await deleteEnvironmentIpAccess({
          path: { project_id: projectId, environment_id: environmentId },
          throwOnError: true,
        })
      } else {
        await deleteProjectIpAccess({
          path: { project_id: projectId },
          throwOnError: true,
        })
      }
    },
    onSuccess: () => {
      setConfirmRemove(false)
      setProbeResult(null)
      queryClient.invalidateQueries({ queryKey })
      toast.success(
        isEnvironment
          ? 'Environment list removed; it now follows the project list'
          : 'IP access list removed; the project is open again'
      )
    },
    onError: (error) => {
      setConfirmRemove(false)
      toast.error(problemDetail(error, 'Failed to remove the IP access list'))
    },
  })

  const probeMutation = useMutation({
    mutationFn: async (body: {
      ip: string
      mode: IpAccessMode
      cidrs: { cidr: string; description?: string | null }[]
    }) => {
      const result = isEnvironment
        ? await testEnvironmentIpAccess({
            path: { project_id: projectId, environment_id: environmentId },
            body,
            throwOnError: true,
          })
        : await testProjectIpAccess({
            path: { project_id: projectId },
            body,
            throwOnError: true,
          })
      return result.data
    },
    onSuccess: setProbeResult,
    onError: (error) => {
      setProbeResult(null)
      toast.error(problemDetail(error, 'Could not evaluate that address'))
    },
  })

  const toRequest = (
    values: IpAccessFormValues,
    acknowledgeLockout: boolean
  ): SetProjectIpAccessRequest => ({
    mode: values.mode,
    cidrs: values.cidrs.map((entry) => ({
      cidr: entry.cidr.trim(),
      description: entry.description?.trim() ? entry.description.trim() : null,
    })),
    acknowledge_lockout: acknowledgeLockout,
  })

  const onSubmit = form.handleSubmit((values) => {
    save.mutate(toRequest(values, false))
  })

  const runProbe = () => {
    const address = probe.trim()
    if (!address) return
    const values = form.getValues()
    // Evaluate the draft as shown so the answer matches what Save would do;
    // an invalid draft falls back to the stored policy.
    const draft = ipAccessFormSchema.safeParse(values)
    probeMutation.mutate({
      ip: address,
      ...(draft.success
        ? {
            mode: draft.data.mode,
            cidrs: draft.data.cidrs.map((entry) => ({
              cidr: entry.cidr.trim(),
              description: entry.description?.trim() || null,
            })),
          }
        : ({} as { mode: IpAccessMode; cidrs: [] })),
    })
  }

  const yourIp = policy?.your_ip ?? null
  const canAddPresets = !hasAllPresets(currentCidrs, TAILSCALE_PRESETS)
  const yourIpListed =
    yourIp !== null &&
    currentCidrs.some((entry) => entry.cidr.trim() === hostCidr(yourIp))
  const atLimit = fields.length >= MAX_CIDRS
  const cidrsError =
    form.formState.errors.cidrs?.message ??
    form.formState.errors.cidrs?.root?.message

  const addPresets = () => {
    const existing = new Set(
      currentCidrs.map((entry) => entry.cidr.trim().toLowerCase())
    )
    TAILSCALE_PRESETS.filter(
      (preset) => !existing.has(preset.cidr.toLowerCase())
    ).forEach((preset) =>
      append({ cidr: preset.cidr, description: preset.description ?? '' })
    )
  }

  const title = isEnvironment ? 'IP access for this environment' : 'IP access'
  const description = isEnvironment
    ? 'Limit which client addresses can reach this environment. Overrides the project list.'
    : 'Allow only, or block, specific IP addresses and ranges for every environment of this project.'

  if (policyQuery.isPending) {
    return (
      <SettingsSection title={title} description={description} icon={Network}>
        <div className="space-y-3" aria-busy="true">
          <Skeleton className="h-5 w-56" />
          <Skeleton className="h-16 w-full" />
          <Skeleton className="h-9 w-full" />
        </div>
      </SettingsSection>
    )
  }

  if (policyQuery.isError || !policy) {
    return (
      <SettingsSection title={title} description={description} icon={Network}>
        <ErrorAlert
          title="Could not load the IP access list"
          description={problemDetail(
            policyQuery.error,
            'The IP access list could not be loaded.'
          )}
          retry={() => policyQuery.refetch()}
          retrying={policyQuery.isFetching}
        />
      </SettingsSection>
    )
  }

  return (
    <SettingsSection title={title} description={description} icon={Network}>
      <form onSubmit={onSubmit} className="space-y-5" noValidate>
        <div className="flex flex-wrap items-center gap-2">
          <Badge variant={policy.mode === 'disabled' ? 'secondary' : 'default'}>
            {policySummary(policy.mode, policy.cidrs.length)}
          </Badge>
          {isEnvironment && !policy.configured && (
            <span className="text-sm text-muted-foreground">
              No list of its own: this environment follows the project list.
            </span>
          )}
        </div>

        <Controller
          control={form.control}
          name="mode"
          render={({ field }) => (
            <RadioGroup
              value={field.value}
              onValueChange={field.onChange}
              className="grid grid-cols-1 gap-2 md:grid-cols-3"
              aria-label="IP access mode"
            >
              {MODES.map((option) => (
                <Label
                  key={option.value}
                  htmlFor={`ip-access-mode-${option.value}`}
                  className="flex cursor-pointer items-start gap-3 rounded-md border p-3 font-normal has-[[data-state=checked]]:border-primary"
                >
                  <RadioGroupItem
                    id={`ip-access-mode-${option.value}`}
                    value={option.value}
                    className="mt-0.5"
                  />
                  <span className="space-y-0.5">
                    <span className="block text-sm font-medium">
                      {modeLabel(option.value)}
                    </span>
                    <span className="block text-sm text-muted-foreground">
                      {option.hint}
                    </span>
                  </span>
                </Label>
              ))}
            </RadioGroup>
          )}
        />

        <div className="space-y-3">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <Label>
              Addresses and ranges{' '}
              <span className="font-normal text-muted-foreground">
                ({fields.length} / {MAX_CIDRS})
              </span>
            </Label>
            <div className="flex flex-wrap gap-2">
              {yourIp && !yourIpListed && (
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  disabled={atLimit}
                  onClick={() =>
                    append({
                      cidr: hostCidr(yourIp),
                      description: 'My current address',
                    })
                  }
                >
                  <UserRound className="mr-2 h-4 w-4" />
                  Add my IP ({yourIp})
                </Button>
              )}
              {canAddPresets && (
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  disabled={atLimit}
                  onClick={addPresets}
                >
                  <Network className="mr-2 h-4 w-4" />
                  Add Tailscale ranges
                </Button>
              )}
              <Button
                type="button"
                variant="outline"
                size="sm"
                disabled={atLimit}
                onClick={() => append({ cidr: '', description: '' })}
              >
                <Plus className="mr-2 h-4 w-4" />
                Add range
              </Button>
            </div>
          </div>

          {fields.length === 0 ? (
            <p className="rounded-md border border-dashed p-4 text-sm text-muted-foreground">
              {mode === 'allow'
                ? 'An allow list needs at least one entry, e.g. 203.0.113.0/24, or 2001:db8::/32 for IPv6. Use "Add Tailscale ranges" to admit your tailnet.'
                : 'No addresses yet. Enter a single address (203.0.113.9) or a range (203.0.113.0/24, 2001:db8::/32).'}
            </p>
          ) : (
            <ul className="space-y-2">
              {fields.map((field, index) => {
                const rowErrors = form.formState.errors.cidrs?.[index]
                return (
                  <li
                    key={field.id}
                    className="grid grid-cols-1 gap-2 sm:grid-cols-[minmax(0,1fr)_minmax(0,1fr)_auto]"
                  >
                    <div className="space-y-1">
                      <Input
                        aria-label={`Address or range ${index + 1}`}
                        aria-invalid={rowErrors?.cidr ? true : undefined}
                        placeholder="203.0.113.0/24"
                        className="font-mono"
                        {...form.register(`cidrs.${index}.cidr` as const)}
                      />
                      {rowErrors?.cidr?.message && (
                        <p className="text-sm text-destructive">
                          {rowErrors.cidr.message}
                        </p>
                      )}
                    </div>
                    <div className="space-y-1">
                      <Input
                        aria-label={`Note for range ${index + 1}`}
                        aria-invalid={rowErrors?.description ? true : undefined}
                        placeholder="Note (optional)"
                        maxLength={MAX_DESCRIPTION_LENGTH}
                        {...form.register(
                          `cidrs.${index}.description` as const
                        )}
                      />
                      {rowErrors?.description?.message && (
                        <p className="text-sm text-destructive">
                          {rowErrors.description.message}
                        </p>
                      )}
                    </div>
                    <Button
                      type="button"
                      variant="ghost"
                      size="icon"
                      aria-label={`Remove range ${index + 1}`}
                      onClick={() => remove(index)}
                    >
                      <Trash2 className="h-4 w-4" />
                    </Button>
                  </li>
                )
              })}
            </ul>
          )}
          {cidrsError && (
            <p className="text-sm text-destructive" role="alert">
              {cidrsError}
            </p>
          )}
        </div>

        {mode !== 'disabled' && yourIp && (
          <Alert>
            <CircleAlert className="h-4 w-4" />
            <AlertTitle>Your address: {yourIp}</AlertTitle>
            <AlertDescription>
              That is how the console sees you. The {scopeNoun} sees clients the
              same way only when both are reached over the same network path;
              test an address below before relying on a list. You are asked to
              confirm if a save would lock this address out.
            </AlertDescription>
          </Alert>
        )}

        <div className="flex flex-col gap-2 sm:flex-row sm:items-end">
          <div className="min-w-0 flex-1 space-y-1">
            <Label htmlFor={`ip-access-probe-${environmentId ?? 'project'}`}>
              Try an address
            </Label>
            <Input
              id={`ip-access-probe-${environmentId ?? 'project'}`}
              value={probe}
              placeholder="100.101.102.103"
              className="font-mono"
              onChange={(event) => setProbe(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === 'Enter') {
                  event.preventDefault()
                  runProbe()
                }
              }}
            />
          </div>
          <Button
            type="button"
            variant="outline"
            disabled={probe.trim() === '' || probeMutation.isPending}
            onClick={runProbe}
          >
            <Search className="mr-2 h-4 w-4" />
            {probeMutation.isPending ? 'Checking…' : 'Check'}
          </Button>
        </div>
        {probeResult && (
          <p
            className={
              probeResult.allowed
                ? 'text-sm text-foreground'
                : 'text-sm text-destructive'
            }
            role="status"
          >
            {probeResult.allowed ? 'Allowed' : 'Blocked'} — {probeResult.reason}
          </p>
        )}

        <div className="flex flex-wrap items-center gap-2">
          <Button
            type="submit"
            disabled={!form.formState.isDirty || save.isPending}
          >
            {save.isPending ? 'Saving…' : 'Save IP access list'}
          </Button>
          {policy.configured && (
            <Button
              type="button"
              variant="outline"
              disabled={remove_.isPending}
              onClick={() => setConfirmRemove(true)}
            >
              {isEnvironment ? 'Use project list' : 'Remove list'}
            </Button>
          )}
          {form.formState.isDirty && (
            <Button
              type="button"
              variant="ghost"
              onClick={() => {
                form.reset({
                  mode: policy.mode,
                  cidrs: policy.cidrs.map((entry) => ({
                    cidr: entry.cidr,
                    description: entry.description ?? '',
                  })),
                })
                replace(
                  policy.cidrs.map((entry) => ({
                    cidr: entry.cidr,
                    description: entry.description ?? '',
                  }))
                )
              }}
            >
              Discard changes
            </Button>
          )}
        </div>
      </form>

      <AlertDialog
        open={lockout !== null}
        onOpenChange={(open) => {
          if (!open) setLockout(null)
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>This would lock you out</AlertDialogTitle>
            <AlertDialogDescription>{lockout?.detail}</AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Keep editing</AlertDialogCancel>
            <AlertDialogAction
              onClick={() => {
                if (lockout) {
                  save.mutate({ ...lockout.request, acknowledge_lockout: true })
                }
              }}
            >
              Save anyway
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      <AlertDialog open={confirmRemove} onOpenChange={setConfirmRemove}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>
              {isEnvironment ? 'Use the project list?' : 'Remove this list?'}
            </AlertDialogTitle>
            <AlertDialogDescription>
              {isEnvironment
                ? 'This environment will follow the project-wide list again.'
                : 'Every environment of this project will be open to all addresses again.'}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction onClick={() => remove_.mutate()}>
              {isEnvironment ? 'Use project list' : 'Remove list'}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </SettingsSection>
  )
}
