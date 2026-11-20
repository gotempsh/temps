// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  createTargetMonitorMutation,
  getMonitorProbeLocationsOptions,
  updateTargetMonitorMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type { MonitorResponse, ProbeLocationsResponse } from '@/api/client'
import { Button } from '@/components/ui/button'
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
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Switch } from '@/components/ui/switch'
import { Textarea } from '@/components/ui/textarea'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Plus, Trash2 } from 'lucide-react'
import { useEffect } from 'react'
import { useFieldArray, useForm, useWatch } from 'react-hook-form'
import { Link } from 'react-router'
import { toast } from 'sonner'
import {
  createTargetMonitorRequest,
  DNS_RECORD_TYPES,
  HTTP_METHODS,
  locationLabel,
  newTargetMonitorValues,
  TARGET_KIND_COPY,
  TARGET_KINDS,
  targetMonitorSchema,
  targetMonitorValues,
  updateTargetMonitorRequest,
  type TargetMonitorValues,
} from './target-monitor'

/** Monitor a URL, port, certificate or DNS record outside Temps (ADR 108 D3, D4). */
export function TargetMonitorDialog({
  projectId,
  open,
  editing,
  onOpenChange,
}: {
  projectId: number
  open: boolean
  /** A target monitor to change; `null` creates one. */
  editing: MonitorResponse | null
  onOpenChange: (open: boolean) => void
}) {
  const queryClient = useQueryClient()
  const form = useForm<TargetMonitorValues>({
    resolver: zodResolver(targetMonitorSchema),
    defaultValues: newTargetMonitorValues(),
  })
  const headers = useFieldArray({ control: form.control, name: 'headers' })
  useEffect(() => {
    if (open) {
      form.reset(
        editing ? targetMonitorValues(editing) : newTargetMonitorValues()
      )
    }
  }, [open, editing, form])

  const onSaved = async (message: string) => {
    await queryClient.invalidateQueries({ queryKey: [{ _id: 'listMonitors' }] })
    toast.success(message)
    onOpenChange(false)
  }
  const create = useMutation({
    ...createTargetMonitorMutation(),
    meta: { errorTitle: 'Failed to create monitor' },
    onSuccess: () =>
      onSaved('Monitor created. The first check runs within a minute.'),
  })
  const update = useMutation({
    ...updateTargetMonitorMutation(),
    meta: { errorTitle: 'Failed to save monitor' },
    onSuccess: () =>
      onSaved('Monitor saved. The next check uses the new settings.'),
  })
  const pending = create.isPending || update.isPending
  const kind = useWatch({ control: form.control, name: 'kind' })
  const replaceRequest = useWatch({
    control: form.control,
    name: 'replaceRequest',
  })
  const copy = TARGET_KIND_COPY[kind]
  const probeLocations = useQuery({
    ...getMonitorProbeLocationsOptions({ path: { project_id: projectId } }),
    enabled: open,
  })

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>
            {editing ? 'Edit monitor' : 'Monitor something outside Temps'}
          </DialogTitle>
          <DialogDescription>
            A third-party API, a site on another host, a database port. The
            target must have a public address.
          </DialogDescription>
        </DialogHeader>
        <Form {...form}>
          <form
            className="space-y-4"
            onSubmit={form.handleSubmit((values) => {
              if (editing) {
                update.mutate({
                  path: { project_id: projectId, monitor_id: editing.id },
                  body: updateTargetMonitorRequest(values),
                })
              } else {
                create.mutate({
                  path: { project_id: projectId },
                  body: createTargetMonitorRequest(values),
                })
              }
            })}
          >
            <FormField
              control={form.control}
              name="kind"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>What to check</FormLabel>
                  <div
                    role="radiogroup"
                    aria-label="What to check"
                    className="grid grid-cols-1 gap-2 sm:grid-cols-2"
                  >
                    {TARGET_KINDS.map((option) => (
                      <button
                        key={option}
                        type="button"
                        role="radio"
                        aria-checked={field.value === option}
                        onClick={() => field.onChange(option)}
                        className={
                          'rounded-lg border p-2.5 text-left text-xs transition-colors hover:bg-muted/50 ' +
                          (field.value === option
                            ? 'border-primary bg-primary/5'
                            : '')
                        }
                      >
                        <span className="block text-sm font-medium">
                          {TARGET_KIND_COPY[option].label}
                        </span>
                        <span className="text-muted-foreground">
                          {TARGET_KIND_COPY[option].hint}
                        </span>
                      </button>
                    ))}
                  </div>
                </FormItem>
              )}
            />
            <div className="grid grid-cols-1 gap-4 sm:grid-cols-[2fr_1fr]">
              <FormField
                control={form.control}
                name="target"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>{copy.targetLabel}</FormLabel>
                    <FormControl>
                      <Input
                        placeholder={copy.placeholder}
                        autoComplete="off"
                        {...field}
                      />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
              <FormField
                control={form.control}
                name="name"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Name</FormLabel>
                    <FormControl>
                      <Input
                        placeholder="Payments API"
                        maxLength={100}
                        {...field}
                      />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
            </div>

            {kind === 'http' && (
              <>
                <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
                  <FormField
                    control={form.control}
                    name="method"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Method</FormLabel>
                        <Select
                          value={field.value}
                          onValueChange={field.onChange}
                        >
                          <FormControl>
                            <SelectTrigger className="w-full">
                              <SelectValue />
                            </SelectTrigger>
                          </FormControl>
                          <SelectContent>
                            {HTTP_METHODS.map((method) => (
                              <SelectItem key={method} value={method}>
                                {method}
                              </SelectItem>
                            ))}
                          </SelectContent>
                        </Select>
                      </FormItem>
                    )}
                  />
                  <FormField
                    control={form.control}
                    name="expectedStatus"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Expected status</FormLabel>
                        <FormControl>
                          <Input placeholder="2xx or 3xx" {...field} />
                        </FormControl>
                        <FormDescription>
                          Codes or classes, e.g. 200, 204
                        </FormDescription>
                        <FormMessage />
                      </FormItem>
                    )}
                  />
                </div>
                <div className="grid grid-cols-1 gap-4 sm:grid-cols-[2fr_1fr]">
                  <FormField
                    control={form.control}
                    name="keyword"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Body keyword (optional)</FormLabel>
                        <FormControl>
                          <Input
                            placeholder='"status":"ok"'
                            maxLength={200}
                            {...field}
                          />
                        </FormControl>
                        <FormMessage />
                      </FormItem>
                    )}
                  />
                  <FormField
                    control={form.control}
                    name="keywordPresent"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Keyword must be</FormLabel>
                        <Select
                          value={field.value ? 'present' : 'absent'}
                          onValueChange={(value) =>
                            field.onChange(value === 'present')
                          }
                        >
                          <FormControl>
                            <SelectTrigger className="w-full">
                              <SelectValue />
                            </SelectTrigger>
                          </FormControl>
                          <SelectContent>
                            <SelectItem value="present">Present</SelectItem>
                            <SelectItem value="absent">Absent</SelectItem>
                          </SelectContent>
                        </Select>
                      </FormItem>
                    )}
                  />
                </div>
              </>
            )}

            {(kind === 'http' || kind === 'tcp') && (
              <FormField
                control={form.control}
                name="degradedAfterMs"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>
                      Degraded when slower than (ms, optional)
                    </FormLabel>
                    <FormControl>
                      <Input
                        inputMode="numeric"
                        placeholder="1500"
                        {...field}
                      />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
            )}

            {kind === 'tls' && (
              <FormField
                control={form.control}
                name="warnDays"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>
                      Degraded when it expires within (days)
                    </FormLabel>
                    <FormControl>
                      <Input
                        type="number"
                        min={1}
                        max={90}
                        {...field}
                        onChange={(event) =>
                          field.onChange(Number(event.target.value))
                        }
                      />
                    </FormControl>
                    <FormDescription>
                      An expired or untrusted certificate is down.
                    </FormDescription>
                    <FormMessage />
                  </FormItem>
                )}
              />
            )}

            {kind === 'dns' && (
              <div className="grid grid-cols-1 gap-4 sm:grid-cols-[1fr_2fr]">
                <FormField
                  control={form.control}
                  name="recordType"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Record</FormLabel>
                      <Select
                        value={field.value}
                        onValueChange={field.onChange}
                      >
                        <FormControl>
                          <SelectTrigger className="w-full">
                            <SelectValue />
                          </SelectTrigger>
                        </FormControl>
                        <SelectContent>
                          {DNS_RECORD_TYPES.map((type) => (
                            <SelectItem key={type} value={type}>
                              {type}
                            </SelectItem>
                          ))}
                        </SelectContent>
                      </Select>
                    </FormItem>
                  )}
                />
                <FormField
                  control={form.control}
                  name="expectedValues"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Expected values (optional)</FormLabel>
                      <FormControl>
                        <Input
                          placeholder="203.0.113.10, 203.0.113.11"
                          {...field}
                        />
                      </FormControl>
                      <FormDescription>
                        Empty: any answer is up.
                      </FormDescription>
                      <FormMessage />
                    </FormItem>
                  )}
                />
              </div>
            )}

            {kind === 'http' && (
              <div className="space-y-3 rounded-lg border p-3">
                <div className="flex items-center justify-between gap-2">
                  <span className="text-sm font-medium">
                    Request headers and body
                  </span>
                  {editing && (
                    <label className="flex items-center gap-2 text-xs text-muted-foreground">
                      <Switch
                        checked={replaceRequest}
                        onCheckedChange={(next) =>
                          form.setValue('replaceRequest', next)
                        }
                        aria-label="Replace the stored headers and body"
                      />
                      Replace stored
                    </label>
                  )}
                </div>
                <p className="text-xs text-muted-foreground">
                  Stored encrypted. Values are never shown again
                  {editing && !replaceRequest
                    ? '; the stored ones are kept.'
                    : '.'}
                </p>
                {(!editing || replaceRequest) && (
                  <>
                    {headers.fields.map((item, index) => (
                      <div
                        key={item.id}
                        className="flex flex-col gap-2 sm:flex-row"
                      >
                        <Input
                          aria-label="Header name"
                          placeholder="Authorization"
                          className="sm:w-40"
                          {...form.register(`headers.${index}.name`)}
                        />
                        <Input
                          aria-label="Header value"
                          type="password"
                          autoComplete="off"
                          placeholder="Bearer …"
                          {...form.register(`headers.${index}.value`)}
                        />
                        <Button
                          type="button"
                          variant="ghost"
                          size="icon"
                          aria-label="Remove header"
                          onClick={() => headers.remove(index)}
                        >
                          <Trash2 className="size-4" />
                        </Button>
                      </div>
                    ))}
                    <Button
                      type="button"
                      variant="outline"
                      size="sm"
                      onClick={() => headers.append({ name: '', value: '' })}
                    >
                      <Plus className="mr-2 size-4" />
                      Add header
                    </Button>
                    <FormField
                      control={form.control}
                      name="body"
                      render={({ field }) => (
                        <FormItem>
                          <FormLabel className="text-xs">
                            Body (optional)
                          </FormLabel>
                          <FormControl>
                            <Textarea
                              rows={2}
                              placeholder='{"ping":true}'
                              {...field}
                            />
                          </FormControl>
                          <FormMessage />
                        </FormItem>
                      )}
                    />
                  </>
                )}
              </div>
            )}

            <FormField
              control={form.control}
              name="intervalSeconds"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Check every (seconds)</FormLabel>
                  <FormControl>
                    <Input
                      type="number"
                      min={30}
                      {...field}
                      onChange={(event) =>
                        field.onChange(Number(event.target.value))
                      }
                    />
                  </FormControl>
                  <FormMessage />
                </FormItem>
              )}
            />
            <FormField
              control={form.control}
              name="probeFromNodes"
              render={({ field }) => (
                <FormItem className="flex flex-row items-start justify-between gap-4 rounded-md border p-3">
                  <div className="space-y-1">
                    <FormLabel>Check from worker nodes too</FormLabel>
                    <FormDescription>
                      <ProbeLocationsHint
                        isPending={probeLocations.isPending}
                        isError={probeLocations.isError}
                        data={probeLocations.data}
                      />
                    </FormDescription>
                  </div>
                  <FormControl>
                    <Switch
                      checked={field.value}
                      onCheckedChange={field.onChange}
                    />
                  </FormControl>
                </FormItem>
              )}
            />
            <DialogFooter>
              <Button
                type="button"
                variant="outline"
                onClick={() => onOpenChange(false)}
              >
                Cancel
              </Button>
              <Button type="submit" disabled={pending}>
                {pending ? 'Saving...' : editing ? 'Save' : 'Create monitor'}
              </Button>
            </DialogFooter>
          </form>
        </Form>
      </DialogContent>
    </Dialog>
  )
}

/** Where the check would run from, or what is missing for a second place. */
function ProbeLocationsHint({
  isPending,
  isError,
  data,
}: {
  isPending: boolean
  isError: boolean
  data: ProbeLocationsResponse | undefined
}) {
  if (isPending) return <>Looking for worker nodes…</>
  if (isError || !data) {
    return (
      <>
        Down only when most locations agree. Worker nodes could not be listed
        right now.
      </>
    )
  }
  if (!data.configured) {
    return (
      <>
        {data.reason}{' '}
        {data.setup_path && (
          <Link className="underline" to={data.setup_path}>
            Add a worker node
          </Link>
        )}
      </>
    )
  }
  return (
    <>
      Also from {data.locations.slice(1).map(locationLabel).join(', ')}. Down
      only when most locations agree.
    </>
  )
}
