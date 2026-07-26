import {
  createAlertRuleMutation,
  getAlertRuleOptions,
  updateAlertRuleMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { CreateAlertRuleRequest } from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
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
import { Skeleton } from '@/components/ui/skeleton'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { ArrowLeft } from 'lucide-react'
import { useMemo } from 'react'
import { useForm } from 'react-hook-form'
import { useNavigate, useParams } from 'react-router'
import { toast } from 'sonner'
import { z } from 'zod'

const TRIGGER_TYPES = [
  { value: 'new_issue', label: 'New Issue', description: 'Fires once when a new error group is first created. Does not fire again for subsequent events in the same group.' },
  { value: 'regression', label: 'Regression', description: 'Fires when a new event arrives for an error group that was previously marked as "resolved" or "ignored". The group is automatically re-opened.' },
  { value: 'frequency', label: 'Frequency', description: 'Fires when the number of events in an error group exceeds the configured threshold within the time window. Can fire repeatedly after cooldown expires.' },
  { value: 'new_user', label: 'New User Affected', description: 'Fires when the first event with user context (user ID, email, or visitor ID) is added to an error group.' },
  { value: 'user_count', label: 'User Count', description: 'Fires when the number of unique users affected by an error group reaches the configured threshold.' },
  { value: 'status_change', label: 'Status Change', description: 'Fires when an error group status is manually changed (e.g. resolved, assigned, ignored).' },
] as const

const PRIORITIES = [
  { value: 'Low', label: 'Low' },
  { value: 'Normal', label: 'Normal' },
  { value: 'High', label: 'High' },
  { value: 'Critical', label: 'Critical' },
] as const

const alertRuleSchema = z.object({
  name: z.string().min(1, 'Name is required'),
  trigger_type: z.string().min(1, 'Trigger type is required'),
  trigger_config: z.object({
    count: z.number().min(1).optional(),
    window_minutes: z.number().min(1).optional(),
    threshold: z.number().min(1).optional(),
  }).optional(),
  cooldown_minutes: z.number().min(0),
  notification_priority: z.string(),
  environment_filter: z.number().nullable().optional(),
  error_level_filter: z.string().nullable().optional(),
  enabled: z.boolean(),
})

type AlertRuleFormData = z.infer<typeof alertRuleSchema>

function needsConfig(triggerType: string): boolean {
  return ['frequency', 'user_count'].includes(triggerType)
}

interface AlertRuleFormProps {
  projectId: number
}

export function AlertRuleForm({ projectId }: AlertRuleFormProps) {
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const { ruleId } = useParams()
  const isEditing = !!ruleId

  const { data: existingRule, isLoading: ruleLoading } = useQuery({
    ...getAlertRuleOptions({
      path: { project_id: projectId, rule_id: Number(ruleId) },
    }),
    enabled: isEditing,
  })

  const defaultValues = useMemo<AlertRuleFormData>(() => {
    if (existingRule) {
      const config = (existingRule.trigger_config ?? {}) as Record<string, unknown>
      return {
        name: existingRule.name,
        trigger_type: existingRule.trigger_type,
        trigger_config: {
          count: (config.count as number) ?? undefined,
          window_minutes: (config.window_minutes as number) ?? undefined,
          threshold: (config.threshold as number) ?? undefined,
        },
        cooldown_minutes: existingRule.cooldown_minutes,
        notification_priority: existingRule.notification_priority,
        environment_filter: existingRule.environment_filter ?? null,
        error_level_filter: existingRule.error_level_filter ?? null,
        enabled: existingRule.enabled,
      }
    }
    return {
      name: '',
      trigger_type: 'new_issue',
      trigger_config: {},
      cooldown_minutes: 60,
      notification_priority: 'High',
      environment_filter: null,
      error_level_filter: null,
      enabled: true,
    }
  }, [existingRule])

  const form = useForm<AlertRuleFormData>({
    resolver: zodResolver(alertRuleSchema),
    values: defaultValues,
  })

  const watchedTriggerType = form.watch('trigger_type')

  const createMutation = useMutation({
    ...createAlertRuleMutation(),
    meta: { errorTitle: 'Failed to create alert rule' },
    onSuccess: () => {
      toast.success('Alert rule created')
      queryClient.invalidateQueries({ predicate: (query) => (query.queryKey[0] as Record<string, unknown>)?._id === 'listAlertRules' })
      navigate(-1)
    },
  })

  const updateMutation = useMutation({
    ...updateAlertRuleMutation(),
    meta: { errorTitle: 'Failed to update alert rule' },
    onSuccess: () => {
      toast.success('Alert rule updated')
      queryClient.invalidateQueries({ predicate: (query) => (query.queryKey[0] as Record<string, unknown>)?._id === 'listAlertRules' })
      navigate(-1)
    },
  })

  const isMutating = createMutation.isPending || updateMutation.isPending

  const onSubmit = async (data: AlertRuleFormData) => {
    const triggerConfig = needsConfig(data.trigger_type) ? data.trigger_config : undefined

    if (isEditing) {
      await updateMutation.mutateAsync({
        path: { project_id: projectId, rule_id: Number(ruleId) },
        body: {
          name: data.name,
          trigger_type: data.trigger_type,
          trigger_config: triggerConfig,
          cooldown_minutes: data.cooldown_minutes,
          notification_priority: data.notification_priority,
          environment_filter: data.environment_filter,
          error_level_filter: data.error_level_filter,
          enabled: data.enabled,
        },
      })
    } else {
      await createMutation.mutateAsync({
        path: { project_id: projectId },
        body: {
          name: data.name,
          trigger_type: data.trigger_type,
          trigger_config: triggerConfig,
          cooldown_minutes: data.cooldown_minutes,
          notification_priority: data.notification_priority,
          environment_filter: data.environment_filter,
          error_level_filter: data.error_level_filter,
          enabled: data.enabled,
        } as CreateAlertRuleRequest,
      })
    }
  }

  if (isEditing && ruleLoading) {
    return (
      <div className="space-y-6 max-w-2xl mx-auto">
        <div className="flex items-center gap-4">
          <Skeleton className="h-9 w-9" />
          <Skeleton className="h-8 w-48" />
        </div>
        <Skeleton className="h-[600px] w-full" />
      </div>
    )
  }

  return (
    <div className="space-y-6 max-w-2xl mx-auto">
      <div className="flex items-center gap-4">
        <Button variant="ghost" size="icon" onClick={() => navigate(-1)}>
          <ArrowLeft className="h-4 w-4" />
        </Button>
        <div>
          <h2 className="text-lg font-semibold">
            {isEditing ? 'Edit Alert Rule' : 'Create Alert Rule'}
          </h2>
          <p className="text-sm text-muted-foreground">
            {isEditing
              ? 'Update the alert rule configuration.'
              : 'Configure a new error alert rule.'}
          </p>
        </div>
      </div>

      <Card>
        <CardHeader>
          <CardTitle>Rule Configuration</CardTitle>
          <CardDescription>
            Define when and how this alert should fire.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <Form {...form}>
            <form onSubmit={form.handleSubmit(onSubmit)} className="space-y-6">
              <FormField
                control={form.control}
                name="name"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Name</FormLabel>
                    <FormControl>
                      <Input placeholder="e.g. Critical error spike" {...field} />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />

              <FormField
                control={form.control}
                name="trigger_type"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Trigger Type</FormLabel>
                    <Select onValueChange={field.onChange} value={field.value}>
                      <FormControl>
                        <SelectTrigger>
                          <SelectValue placeholder="Select trigger" />
                        </SelectTrigger>
                      </FormControl>
                      <SelectContent>
                        {TRIGGER_TYPES.map((t) => (
                          <SelectItem key={t.value} value={t.value}>
                            {t.label}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                    <FormDescription>
                      {TRIGGER_TYPES.find((t) => t.value === field.value)?.description}
                    </FormDescription>
                    <FormMessage />
                  </FormItem>
                )}
              />

              {needsConfig(watchedTriggerType) && (
                <div className="space-y-4 rounded-md border p-4">
                  <p className="text-sm font-medium">Trigger Configuration</p>
                  {watchedTriggerType === 'frequency' && (
                    <>
                      <FormField
                        control={form.control}
                        name="trigger_config.count"
                        render={({ field }) => (
                          <FormItem>
                            <FormLabel>Event Count</FormLabel>
                            <FormControl>
                              <Input
                                type="number"
                                placeholder="100"
                                value={field.value ?? ''}
                                onChange={(e) =>
                                  field.onChange(e.target.value ? Number(e.target.value) : undefined)
                                }
                              />
                            </FormControl>
                            <FormDescription>
                              Number of events to trigger the alert
                            </FormDescription>
                            <FormMessage />
                          </FormItem>
                        )}
                      />
                      <FormField
                        control={form.control}
                        name="trigger_config.window_minutes"
                        render={({ field }) => (
                          <FormItem>
                            <FormLabel>Time Window (minutes)</FormLabel>
                            <FormControl>
                              <Input
                                type="number"
                                placeholder="60"
                                value={field.value ?? ''}
                                onChange={(e) =>
                                  field.onChange(e.target.value ? Number(e.target.value) : undefined)
                                }
                              />
                            </FormControl>
                            <FormMessage />
                          </FormItem>
                        )}
                      />
                    </>
                  )}
                  {watchedTriggerType === 'user_count' && (
                    <FormField
                      control={form.control}
                      name="trigger_config.threshold"
                      render={({ field }) => (
                        <FormItem>
                          <FormLabel>User Threshold</FormLabel>
                          <FormControl>
                            <Input
                              type="number"
                              placeholder="10"
                              value={field.value ?? ''}
                              onChange={(e) =>
                                field.onChange(e.target.value ? Number(e.target.value) : undefined)
                              }
                            />
                          </FormControl>
                          <FormDescription>
                            Trigger when this many unique users are affected
                          </FormDescription>
                          <FormMessage />
                        </FormItem>
                      )}
                    />
                  )}
                </div>
              )}

              <FormField
                control={form.control}
                name="notification_priority"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Priority</FormLabel>
                    <Select onValueChange={field.onChange} value={field.value}>
                      <FormControl>
                        <SelectTrigger>
                          <SelectValue />
                        </SelectTrigger>
                      </FormControl>
                      <SelectContent>
                        {PRIORITIES.map((p) => (
                          <SelectItem key={p.value} value={p.value}>
                            {p.label}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                    <FormDescription>
                      Adds a prefix to email subjects (e.g. [CRITICAL]) and is included in webhook/Slack payloads. Use it to filter or sort notifications in your inbox.
                    </FormDescription>
                    <FormMessage />
                  </FormItem>
                )}
              />

              <FormField
                control={form.control}
                name="cooldown_minutes"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Cooldown (minutes)</FormLabel>
                    <FormControl>
                      <Input
                        type="number"
                        {...field}
                        onChange={(e) => field.onChange(Number(e.target.value))}
                      />
                    </FormControl>
                    <FormDescription>
                      Minimum time between notifications for the same rule and error group. After an alert fires, it won't fire again for this group until the cooldown expires and a new matching event arrives.
                    </FormDescription>
                    <FormMessage />
                  </FormItem>
                )}
              />

              <FormField
                control={form.control}
                name="error_level_filter"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Error Level Filter</FormLabel>
                    <Select
                      onValueChange={(v) => field.onChange(v === 'all' ? null : v)}
                      value={field.value ?? 'all'}
                    >
                      <FormControl>
                        <SelectTrigger>
                          <SelectValue placeholder="All levels" />
                        </SelectTrigger>
                      </FormControl>
                      <SelectContent>
                        <SelectItem value="all">All Levels</SelectItem>
                        <SelectItem value="error">Error</SelectItem>
                        <SelectItem value="warning">Warning</SelectItem>
                        <SelectItem value="fatal">Fatal</SelectItem>
                      </SelectContent>
                    </Select>
                    <FormDescription>
                      Only trigger for errors at this level
                    </FormDescription>
                    <FormMessage />
                  </FormItem>
                )}
              />

              <FormField
                control={form.control}
                name="enabled"
                render={({ field }) => (
                  <FormItem className="flex items-center justify-between rounded-lg border p-3">
                    <div>
                      <FormLabel>Enabled</FormLabel>
                      <FormDescription>
                        Activate this rule immediately
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

              <div className="flex items-center gap-3 pt-4">
                <Button type="submit" disabled={isMutating}>
                  {isMutating
                    ? 'Saving...'
                    : isEditing
                      ? 'Update Rule'
                      : 'Create Rule'}
                </Button>
                <Button type="button" variant="outline" onClick={() => navigate(-1)}>
                  Cancel
                </Button>
              </div>
            </form>
          </Form>
        </CardContent>
      </Card>
    </div>
  )
}
