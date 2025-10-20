'use client'

import {
  createBackupScheduleMutation,
  deleteBackupScheduleMutation,
  disableBackupScheduleMutation,
  enableBackupScheduleMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { listBackupSchedules, listS3Sources } from '@/api/client/sdk.gen'
import { BackupScheduleResponse } from '@/api/client/types.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from '@/components/ui/dialog'
import { EmptyState } from '@/components/ui/empty-state'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { cn } from '@/lib/utils'
import { useMutation, useQuery } from '@tanstack/react-query'
import { format } from 'date-fns'
import { DatabaseBackup, MoreHorizontal, Plus } from 'lucide-react'
import { useState } from 'react'
import { toast } from 'sonner'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '../ui/dropdown-menu'

interface ScheduleOption {
  label: string
  value: string
  description: string
  customizable?: boolean
}

const scheduleOptions: ScheduleOption[] = [
  {
    label: 'Every 12 hours',
    value: '0 0 */12 * * *',
    description: 'Runs at 00:00 and 12:00',
  },
  {
    label: 'Daily',
    value: '0 0 0 * * *',
    description: 'Runs every day at midnight',
  },
  {
    label: 'Weekly',
    value: '0 0 0 * * 0',
    description: 'Runs every Sunday at midnight',
  },
  {
    label: 'Monthly',
    value: '0 0 0 1 * *',
    description: 'Runs on the first day of every month at midnight',
  },
  {
    label: 'Custom',
    value: 'custom',
    description: 'Specify a custom cron expression',
    customizable: true,
  },
]

interface NewBackupSchedule {
  name: string
  description?: string
  backup_type: string
  schedule_expression: string
  retention_period: number
  s3_source_id: number
  enabled: boolean
  tags: string[]
}

export function BackupsManagement() {
  const [isCreateDialogOpen, setIsCreateDialogOpen] = useState(false)
  const [newBackup, setNewBackup] = useState<Partial<NewBackupSchedule>>({
    backup_type: 'manual',
    retention_period: 7,
    enabled: true,
    tags: [],
  })
  const [selectedSchedule, setSelectedSchedule] = useState<string>(
    scheduleOptions[1].value
  ) // Default to daily
  const [selectedS3Source, setSelectedS3Source] = useState<string>()
  const [customCron, setCustomCron] = useState('')

  const {
    data: schedules = [],
    refetch: refetchSchedules,
    isLoading: isLoadingSchedules,
  } = useQuery({
    queryKey: ['backupSchedules'],
    queryFn: async () => {
      const { data } = await listBackupSchedules()
      return data
    },
  })

  const { data: s3Sources = [], isLoading: isLoadingS3Sources } = useQuery({
    queryKey: ['s3Sources'],
    queryFn: async () => {
      const { data } = await listS3Sources()
      return data
    },
  })

  const createMutation = useMutation({
    ...createBackupScheduleMutation(),
    meta: {
      errorTitle: 'Failed to create backup schedule',
    },
    onSuccess: () => {
      refetchSchedules()
      setNewBackup({
        backup_type: 'manual',
        retention_period: 7,
        enabled: true,
        tags: [],
      })
      setSelectedS3Source(undefined)
      setIsCreateDialogOpen(false)
      toast.success('Backup schedule created successfully')
    },
  })

  const deleteMutation = useMutation({
    ...deleteBackupScheduleMutation(),
    meta: {
      errorTitle: 'Failed to delete backup schedule',
    },
    onSuccess: () => {
      refetchSchedules()
      toast.success('Backup schedule deleted successfully')
    },
  })

  const disableMutation = useMutation({
    ...disableBackupScheduleMutation(),
    meta: {
      errorTitle: 'Failed to disable backup schedule',
    },
    onSuccess: () => {
      refetchSchedules()
      toast.success('Backup schedule disabled')
    },
  })

  const enableMutation = useMutation({
    ...enableBackupScheduleMutation(),
    meta: {
      errorTitle: 'Failed to enable backup schedule',
    },
    onSuccess: () => {
      refetchSchedules()
      toast.success('Backup schedule enabled')
    },
  })

  const handleScheduleChange = (value: string) => {
    setSelectedSchedule(value)
    if (value !== 'custom') {
      setNewBackup({
        ...newBackup,
        schedule_expression: value,
      })
    }
  }

  const handleCustomCronChange = (value: string) => {
    setCustomCron(value)
    setNewBackup({
      ...newBackup,
      schedule_expression: value,
    })
  }

  const handleCreateBackup = () => {
    if (newBackup.name && selectedS3Source) {
      const schedule_expression =
        selectedSchedule === 'custom' ? customCron : selectedSchedule

      if (!schedule_expression) {
        toast.error(
          'Please select a schedule or enter a custom cron expression'
        )
        return
      }

      createMutation.mutate({
        body: {
          name: newBackup.name,
          description: newBackup.description,
          backup_type: newBackup.backup_type || 'manual',
          schedule_expression,
          retention_period: newBackup.retention_period || 7,
          s3_source_id: parseInt(selectedS3Source),
          enabled: newBackup.enabled ?? true,
          tags: newBackup.tags || [],
        },
      })
    }
  }

  const handleDeleteBackup = (id: number) => {
    deleteMutation.mutate({
      path: { id },
    })
  }

  const handleToggleSchedule = (schedule: BackupScheduleResponse) => {
    if (schedule.enabled) {
      disableMutation.mutate({
        path: { id: schedule.id },
      })
    } else {
      enableMutation.mutate({
        path: { id: schedule.id },
      })
    }
  }

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-lg font-semibold">Backup Schedules</h2>
          <p className="text-sm text-muted-foreground">
            Manage your database backup schedules
          </p>
        </div>
        <Dialog open={isCreateDialogOpen} onOpenChange={setIsCreateDialogOpen}>
          <DialogTrigger asChild>
            <Button>
              <Plus className="mr-2 h-4 w-4" />
              Create Schedule
            </Button>
          </DialogTrigger>
          <DialogContent className="max-h-screen flex flex-col">
            <DialogHeader>
              <DialogTitle>Create New Backup Schedule</DialogTitle>
            </DialogHeader>
            <div className="grid gap-4 py-4 flex-1 overflow-y-auto">
              <div className="grid gap-2">
                <Label htmlFor="name">Schedule Name</Label>
                <Input
                  id="name"
                  placeholder="Daily Backup"
                  value={newBackup.name || ''}
                  onChange={(e) =>
                    setNewBackup({ ...newBackup, name: e.target.value })
                  }
                />
              </div>
              <div className="grid gap-2">
                <Label htmlFor="description">Description (Optional)</Label>
                <Input
                  id="description"
                  placeholder="Daily backup at midnight"
                  value={newBackup.description || ''}
                  onChange={(e) =>
                    setNewBackup({ ...newBackup, description: e.target.value })
                  }
                />
              </div>
              <div className="grid gap-2">
                <Label htmlFor="type">Backup Type</Label>
                <Select
                  value={newBackup.backup_type}
                  onValueChange={(value) =>
                    setNewBackup({ ...newBackup, backup_type: value })
                  }
                >
                  <SelectTrigger>
                    <SelectValue placeholder="Select type" />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="manual">Manual</SelectItem>
                    <SelectItem value="scheduled">Scheduled</SelectItem>
                  </SelectContent>
                </Select>
              </div>
              {newBackup.backup_type === 'scheduled' && (
                <div className="grid gap-2">
                  <Label>Schedule</Label>
                  <RadioGroup
                    value={selectedSchedule}
                    onValueChange={handleScheduleChange}
                    className="gap-4"
                  >
                    {scheduleOptions.map((option) => (
                      <div
                        key={option.value}
                        className="flex items-start space-x-3 space-y-0"
                      >
                        <RadioGroupItem
                          value={option.value}
                          id={option.value}
                        />
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
                  {selectedSchedule === 'custom' && (
                    <div className="mt-4">
                      <Label htmlFor="customCron">Custom Cron Expression</Label>
                      <Input
                        id="customCron"
                        placeholder="0 0 * * *"
                        value={customCron}
                        onChange={(e) => handleCustomCronChange(e.target.value)}
                      />
                      <p className="text-xs text-muted-foreground mt-1">
                        Format: minute hour day month weekday (e.g., &quot;0 0 *
                        * *&quot; for daily at midnight)
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
                  value={newBackup.retention_period || 7}
                  onChange={(e) =>
                    setNewBackup({
                      ...newBackup,
                      retention_period: parseInt(e.target.value),
                    })
                  }
                />
              </div>
              <div className="grid gap-2">
                <Label htmlFor="s3Source">S3 Storage</Label>
                <Select
                  value={selectedS3Source}
                  onValueChange={setSelectedS3Source}
                >
                  <SelectTrigger>
                    <SelectValue placeholder="Select S3 source" />
                  </SelectTrigger>
                  <SelectContent>
                    {s3Sources.map((source) => (
                      <SelectItem key={source.id} value={source.id.toString()}>
                        {source.name} ({source.bucket_name})
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
            </div>
            <DialogFooter className="shrink-0">
              <Button
                onClick={handleCreateBackup}
                disabled={createMutation.isPending}
              >
                {createMutation.isPending ? 'Creating...' : 'Create Schedule'}
              </Button>
            </DialogFooter>
          </DialogContent>
        </Dialog>
      </div>

      <Card>
        <div className="p-4">
          {isLoadingSchedules || isLoadingS3Sources ? (
            <div className="flex items-center justify-center py-6">
              <div className="animate-spin rounded-full h-8 w-8 border-b-2 border-primary"></div>
            </div>
          ) : schedules.length === 0 ? (
            <EmptyState
              icon={DatabaseBackup}
              title="No backup schedules"
              description="Create your first backup schedule to protect your data"
              action={
                <Button onClick={() => setIsCreateDialogOpen(true)}>
                  <Plus className="mr-2 h-4 w-4" />
                  Create Schedule
                </Button>
              }
            />
          ) : (
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Name</TableHead>
                  <TableHead>Type</TableHead>
                  <TableHead>Schedule</TableHead>
                  <TableHead>Status</TableHead>
                  <TableHead>Retention</TableHead>
                  <TableHead>Last Run</TableHead>
                  <TableHead>Next Run</TableHead>
                  <TableHead className="w-[100px]">Actions</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {schedules.map((schedule) => (
                  <TableRow
                    key={schedule.id}
                    className={cn(!schedule.enabled && 'text-muted-foreground')}
                  >
                    <TableCell>
                      <div className="flex items-center gap-3">
                        <DatabaseBackup
                          className={cn(
                            'h-4 w-4',
                            !schedule.enabled && 'text-muted-foreground/60'
                          )}
                        />
                        <div>
                          <div className="font-medium">{schedule.name}</div>
                          {schedule.description && (
                            <div
                              className={cn(
                                'text-sm',
                                !schedule.enabled
                                  ? 'text-muted-foreground/60'
                                  : 'text-muted-foreground'
                              )}
                            >
                              {schedule.description}
                            </div>
                          )}
                        </div>
                      </div>
                    </TableCell>
                    <TableCell>
                      <Badge variant="outline">{schedule.backup_type}</Badge>
                    </TableCell>
                    <TableCell>{schedule.schedule_expression}</TableCell>
                    <TableCell>
                      <Badge
                        variant={schedule.enabled ? 'default' : 'secondary'}
                      >
                        {schedule.enabled ? 'Enabled' : 'Disabled'}
                      </Badge>
                    </TableCell>
                    <TableCell>{schedule.retention_period} days</TableCell>
                    <TableCell>
                      {schedule.last_run
                        ? format(
                            new Date(schedule.last_run),
                            'MMM d, yyyy HH:mm'
                          )
                        : '-'}
                    </TableCell>
                    <TableCell>
                      {schedule.next_run
                        ? format(
                            new Date(schedule.next_run),
                            'MMM d, yyyy HH:mm'
                          )
                        : '-'}
                    </TableCell>
                    <TableCell>
                      <DropdownMenu>
                        <DropdownMenuTrigger asChild>
                          <Button variant="ghost" size="icon">
                            <MoreHorizontal className="h-4 w-4" />
                          </Button>
                        </DropdownMenuTrigger>
                        <DropdownMenuContent align="end">
                          <DropdownMenuItem
                            onClick={() => handleToggleSchedule(schedule)}
                            disabled={disableMutation.isPending}
                          >
                            {schedule.enabled ? 'Disable' : 'Enable'}
                          </DropdownMenuItem>
                          <DropdownMenuSeparator />
                          <DropdownMenuItem
                            onClick={() => handleDeleteBackup(schedule.id)}
                            className="text-destructive"
                            disabled={deleteMutation.isPending}
                          >
                            Delete
                          </DropdownMenuItem>
                        </DropdownMenuContent>
                      </DropdownMenu>
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          )}
        </div>
      </Card>
    </div>
  )
}
