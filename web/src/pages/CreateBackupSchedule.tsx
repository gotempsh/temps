'use client'

/**
 * Standalone page for creating a new backup schedule tied to an S3 source.
 *
 * Route: /backups/s3-sources/:id/schedules/new
 *
 * Replaces the modal-based "Create Backup Schedule" dialog that previously
 * lived inside S3SourceDetail. Using a routed page means the form is never
 * constrained to modal height on small screens.
 */

import {
  attachScheduleServicesMutation,
  createBackupScheduleMutation,
  getS3SourceOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { ScheduleServicesSelector } from '@/components/backups/ScheduleServicesSelector'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group'
import { Switch } from '@/components/ui/switch'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { scheduleOptions } from '@/lib/schedule-options'
import { useMutation, useQuery } from '@tanstack/react-query'
import { ArrowLeft } from 'lucide-react'
import { useEffect, useState } from 'react'
import { Link, Navigate, useNavigate, useParams } from 'react-router-dom'
import { toast } from 'sonner'

interface NewScheduleForm {
  name: string
  description?: string
  backup_type: string
  retention_period: number
  enabled: boolean
  /**
   * Wall-clock timeout in hours. Empty string means "use engine default".
   * Converted to seconds at the API boundary.
   */
  max_runtime_hours: number | ''
}

export function CreateBackupSchedule() {
  const { id } = useParams<{ id: string }>()
  const sourceId = id ? parseInt(id, 10) : undefined
  const navigate = useNavigate()
  const { setBreadcrumbs } = useBreadcrumbs()

  // All hooks before any early return.
  const { data: source } = useQuery({
    ...getS3SourceOptions({ path: { id: sourceId! } }),
    enabled: !!sourceId,
  })

  const [form, setForm] = useState<Partial<NewScheduleForm>>({
    backup_type: 'scheduled',
    retention_period: 7,
    enabled: true,
    max_runtime_hours: '',
  })
  const [selectedPreset, setSelectedPreset] = useState<string>(
    scheduleOptions[1].value,
  )
  const [customCron, setCustomCron] = useState('')
  // 'all' (default) means the schedule backs up every database — including
  // ones created after this schedule. 'specific' means it only backs up the
  // services explicitly picked below. Default chosen to match what most
  // operators want: "back up everything, even when I add a new DB later."
  const [backupMode, setBackupMode] = useState<'all' | 'specific'>('all')
  const [selectedServiceIds, setSelectedServiceIds] = useState<number[]>([])
  // Default on: most operators want the control plane covered. Operators
  // who only use Temps to orchestrate external DB backups can flip it off.
  const [includeControlPlane, setIncludeControlPlane] = useState(true)

  const attachMutation = useMutation({
    ...attachScheduleServicesMutation(),
    meta: { errorTitle: 'Failed to attach services to schedule' },
  })

  // The mutation's generated error type is `ProblemDetails`. Adding an
  // explicit `onError: (err: unknown) => ...` widens that and breaks the
  // typed-options spread above. We rely on the app-wide error toast
  // routed through `meta.errorTitle` instead, mirroring the pattern used
  // by the legacy create-dialog this page replaces.
  const createMutation = useMutation({
    ...createBackupScheduleMutation(),
    meta: { errorTitle: 'Failed to create backup schedule' },
    onSuccess: async (created) => {
      // In 'specific' mode, attach the picked services. In 'all' mode there
      // is nothing to attach — the schedule's `target_all_services` flag is
      // already set on the backend and the fan-out picks every DB at run
      // time.
      if (backupMode === 'specific' && selectedServiceIds.length > 0) {
        try {
          await attachMutation.mutateAsync({
            path: { id: created.id },
            body: { service_ids: selectedServiceIds },
          })
        } catch {
          // Toast already raised by mutation meta; surface partial success.
          toast.warning(
            'Schedule created, but attaching services failed. You can retry from the schedule detail page.',
          )
          navigate(`/backups/s3-sources/${id}`)
          return
        }
      }
      toast.success('Backup schedule created successfully')
      navigate(`/backups/s3-sources/${id}`)
    },
  })

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Backups', href: '/backups' },
      {
        label: source?.name ?? 'S3 Source',
        href: `/backups/s3-sources/${id}`,
      },
      { label: 'New Schedule' },
    ])
  }, [setBreadcrumbs, id, source?.name])

  usePageTitle('New Backup Schedule')

  if (!sourceId) {
    return <Navigate to="/backups" replace />
  }

  function handleSubmit() {
    if (!form.name?.trim()) {
      toast.error('Schedule name is required')
      return
    }

    const schedule_expression =
      selectedPreset === 'custom' ? customCron : selectedPreset

    if (!schedule_expression) {
      toast.error('Please select a schedule or enter a custom cron expression')
      return
    }

    let max_runtime_secs: number | undefined
    if (
      form.max_runtime_hours !== '' &&
      form.max_runtime_hours !== undefined &&
      !Number.isNaN(form.max_runtime_hours)
    ) {
      if (Number(form.max_runtime_hours) <= 0) {
        toast.error('Max runtime must be a positive number of hours')
        return
      }
      max_runtime_secs = Math.round(Number(form.max_runtime_hours) * 3600)
    }

    if (backupMode === 'specific' && selectedServiceIds.length === 0) {
      toast.error(
        'Select at least one database, or switch back to "All databases."',
      )
      return
    }
    if (
      backupMode === 'specific' &&
      selectedServiceIds.length === 0 &&
      !includeControlPlane
    ) {
      toast.error(
        'This schedule would have nothing to back up. Enable the control plane or pick at least one database.',
      )
      return
    }
    if (backupMode === 'all' && !includeControlPlane) {
      // Allowed (all DBs covered), nothing to block here.
    }

    createMutation.mutate({
      body: {
        name: form.name,
        description: form.description,
        backup_type: form.backup_type ?? 'scheduled',
        schedule_expression,
        retention_period: form.retention_period ?? 7,
        s3_source_id: sourceId!,
        enabled: form.enabled ?? true,
        tags: [],
        max_runtime_secs,
        target_all_services: backupMode === 'all',
        include_control_plane: includeControlPlane,
      },
    })
  }

  return (
    <div className="space-y-6 max-w-3xl mx-auto p-4 md:p-6">
      <div className="flex items-center gap-2">
        <Button variant="ghost" size="sm" asChild>
          <Link to={`/backups/s3-sources/${id}`}>
            <ArrowLeft className="mr-2 h-4 w-4" />
            Back to S3 source
          </Link>
        </Button>
      </div>

      <div>
        <h1 className="text-xl font-bold sm:text-2xl">New backup schedule</h1>
        <p className="text-sm text-muted-foreground sm:text-base">
          Run this S3 source's backup on a recurring cron schedule.
        </p>
      </div>

      <div className="space-y-6">
        <div className="grid gap-2">
          <Label htmlFor="name">Schedule Name</Label>
            <Input
              id="name"
              placeholder="Daily Backup"
              value={form.name ?? ''}
              onChange={(e) => setForm({ ...form, name: e.target.value })}
            />
          </div>

          <div className="grid gap-2">
            <Label htmlFor="description">Description (Optional)</Label>
            <Input
              id="description"
              placeholder="Daily backup at midnight"
              value={form.description ?? ''}
              onChange={(e) =>
                setForm({ ...form, description: e.target.value })
              }
            />
          </div>

          <div className="grid gap-2">
            <Label htmlFor="backup-type">Backup Type</Label>
            <Select
              value={form.backup_type ?? 'scheduled'}
              onValueChange={(value) =>
                setForm({ ...form, backup_type: value })
              }
            >
              <SelectTrigger id="backup-type">
                <SelectValue placeholder="Select type" />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="manual">Manual</SelectItem>
                <SelectItem value="scheduled">Scheduled</SelectItem>
              </SelectContent>
            </Select>
          </div>

          {form.backup_type === 'scheduled' && (
            <div className="grid gap-2">
              <Label>Schedule</Label>
              <RadioGroup
                value={selectedPreset}
                onValueChange={setSelectedPreset}
                className="gap-4"
              >
                {scheduleOptions.map((option) => (
                  <div
                    key={option.value}
                    className="flex items-start space-x-3 space-y-0"
                  >
                    <RadioGroupItem value={option.value} id={option.value} />
                    <div className="grid gap-1.5 leading-none">
                      <Label
                        htmlFor={option.value}
                        className="text-sm font-medium leading-none peer-disabled:cursor-not-allowed peer-disabled:opacity-70"
                      >
                        {option.label}
                      </Label>
                      <p className="text-sm text-muted-foreground">
                        {option.description}
                      </p>
                    </div>
                  </div>
                ))}
              </RadioGroup>
              {selectedPreset === 'custom' && (
                <div className="mt-4">
                  <Label htmlFor="custom-cron">Custom Cron Expression</Label>
                  <Input
                    id="custom-cron"
                    placeholder="0 0 * * *"
                    value={customCron}
                    onChange={(e) => setCustomCron(e.target.value)}
                    className="mt-1"
                  />
                  <p className="text-xs text-muted-foreground mt-1">
                    Format: second minute hour day month weekday
                  </p>
                </div>
              )}
            </div>
          )}

          <div className="grid gap-2">
            <Label htmlFor="retention">Retention Period (days)</Label>
            <Input
              id="retention"
              type="number"
              min={1}
              value={form.retention_period ?? 7}
              onChange={(e) =>
                setForm({
                  ...form,
                  retention_period: parseInt(e.target.value, 10),
                })
              }
            />
          </div>

          <div className="grid gap-2">
            <Label>Backup targets</Label>
            <RadioGroup
              value={backupMode}
              onValueChange={(v) => setBackupMode(v as 'all' | 'specific')}
              className="gap-4"
            >
              <div className="flex items-start space-x-3 space-y-0">
                <RadioGroupItem value="all" id="mode-all" />
                <div className="grid gap-1 leading-none">
                  <Label
                    htmlFor="mode-all"
                    className="text-sm font-medium leading-none"
                  >
                    All databases (recommended)
                  </Label>
                  <p className="text-sm text-muted-foreground">
                    Back up every database currently on the host — and any
                    new database you create later, automatically.
                  </p>
                </div>
              </div>
              <div className="flex items-start space-x-3 space-y-0">
                <RadioGroupItem value="specific" id="mode-specific" />
                <div className="grid gap-1 leading-none">
                  <Label
                    htmlFor="mode-specific"
                    className="text-sm font-medium leading-none"
                  >
                    Specific databases
                  </Label>
                  <p className="text-sm text-muted-foreground">
                    Pick the databases this schedule should back up. New
                    databases are not included unless you attach them.
                  </p>
                </div>
              </div>
            </RadioGroup>
            {backupMode === 'specific' && (
              <div className="mt-2 rounded-md border p-2">
                <ScheduleServicesSelector
                  value={selectedServiceIds}
                  onChange={setSelectedServiceIds}
                  disabled={
                    createMutation.isPending || attachMutation.isPending
                  }
                />
              </div>
            )}
            <div className="mt-3 flex items-start justify-between gap-3 rounded-md border p-3">
              <div className="grid gap-1 leading-tight">
                <Label
                  htmlFor="include-control-plane"
                  className="text-sm font-medium"
                >
                  Also back up the Temps control plane
                </Label>
                <p className="text-xs text-muted-foreground">
                  Includes Temps's own database (users, projects, service
                  configs, audit logs, error groups). Recommended unless you
                  use Temps purely as a backup orchestrator for external
                  databases.
                </p>
              </div>
              <Switch
                id="include-control-plane"
                checked={includeControlPlane}
                onCheckedChange={setIncludeControlPlane}
                disabled={
                  createMutation.isPending || attachMutation.isPending
                }
              />
            </div>
          </div>

          <div className="grid gap-2">
            <Label htmlFor="max-runtime">Max runtime (hours)</Label>
            <Input
              id="max-runtime"
              type="number"
              min={1}
              step={1}
              placeholder="auto"
              value={form.max_runtime_hours ?? ''}
              onChange={(e) => {
                const raw = e.target.value
                setForm({
                  ...form,
                  max_runtime_hours: raw === '' ? '' : Number(raw),
                })
              }}
            />
            <p className="text-xs text-muted-foreground">
              Wall-clock ceiling for one backup attempt. Leave empty to use
              the engine default (24h for Postgres, 4h for Redis/MongoDB, 12h
              for S3 mirror).
            </p>
          </div>
      </div>

      <div className="flex justify-end gap-2">
        <Button variant="outline" asChild>
          <Link to={`/backups/s3-sources/${id}`}>Cancel</Link>
        </Button>
        <Button
          onClick={handleSubmit}
          disabled={createMutation.isPending || attachMutation.isPending}
        >
          {createMutation.isPending || attachMutation.isPending
            ? 'Creating…'
            : 'Create schedule'}
        </Button>
      </div>
    </div>
  )
}
