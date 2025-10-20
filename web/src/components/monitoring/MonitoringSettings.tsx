'use client'

import { getPreferences, updatePreferences } from '@/api/client/sdk.gen'
import { NotificationPreferencesResponse } from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
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
import { Tabs, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { zodResolver } from '@hookform/resolvers/zod'
import { useQuery } from '@tanstack/react-query'
import { useForm } from 'react-hook-form'
import { useNavigate, useParams } from 'react-router-dom'
import { toast } from 'sonner'
import { ProvidersManagement } from './ProvidersManagement'
import {
  backupAlertsSchema,
  domainAlertsSchema,
  notificationSettingsSchema,
  projectAlertsSchema,
  routeAlertsSchema,
  type BackupAlertsFormData,
  type DomainAlertsFormData,
  type NotificationSettingsFormData,
  type ProjectAlertsFormData,
  type RouteAlertsFormData,
} from './schemas'

interface AlertComponentProps<T> {
  onSave: (data: T) => Promise<void>
  defaultValues?: Partial<T>
}

function ProjectAlerts({
  onSave,
  defaultValues,
}: AlertComponentProps<ProjectAlertsFormData>) {
  const form = useForm<ProjectAlertsFormData>({
    resolver: zodResolver(projectAlertsSchema),
    defaultValues: {
      deploymentFailures: defaultValues?.deploymentFailures,
      buildErrors: defaultValues?.buildErrors,
      runtimeErrors: {
        enabled: defaultValues?.runtimeErrors?.enabled,
        errorThreshold: defaultValues?.runtimeErrors?.errorThreshold,
        windowMinutes: defaultValues?.runtimeErrors?.windowMinutes,
      },
    },
  })

  const handleSubmit = async (data: ProjectAlertsFormData) => {
    await onSave(data)
    form.reset(data)
  }

  return (
    <Form {...form}>
      <form onSubmit={form.handleSubmit(handleSubmit)} className="space-y-6">
        <div className="space-y-4">
          <h3 className="text-lg font-medium">Project Health</h3>
          <div className="space-y-4">
            <FormField
              control={form.control}
              name="deploymentFailures"
              render={({ field }) => (
                <FormItem className="flex items-center justify-between">
                  <FormLabel>Deployment Failures</FormLabel>
                  <FormControl>
                    <Switch
                      checked={field.value}
                      onCheckedChange={field.onChange}
                    />
                  </FormControl>
                </FormItem>
              )}
            />

            <FormField
              control={form.control}
              name="buildErrors"
              render={({ field }) => (
                <FormItem className="flex items-center justify-between">
                  <FormLabel>Build Errors</FormLabel>
                  <FormControl>
                    <Switch
                      checked={field.value}
                      onCheckedChange={field.onChange}
                    />
                  </FormControl>
                </FormItem>
              )}
            />

            <FormField
              control={form.control}
              name="runtimeErrors.enabled"
              render={({ field }) => (
                <FormItem className="space-y-4">
                  <div className="flex items-center justify-between">
                    <FormLabel>Runtime Errors</FormLabel>
                    <FormControl>
                      <Switch
                        checked={field.value}
                        onCheckedChange={field.onChange}
                      />
                    </FormControl>
                  </div>

                  {field.value && (
                    <div className="grid gap-4 pl-6">
                      <FormField
                        control={form.control}
                        name="runtimeErrors.errorThreshold"
                        render={({ field }) => (
                          <FormItem>
                            <FormLabel>Error Threshold (per minute)</FormLabel>
                            <FormControl>
                              <Input
                                type="number"
                                {...field}
                                onChange={(e) =>
                                  field.onChange(parseInt(e.target.value))
                                }
                              />
                            </FormControl>
                            <FormMessage />
                          </FormItem>
                        )}
                      />

                      <FormField
                        control={form.control}
                        name="runtimeErrors.windowMinutes"
                        render={({ field }) => (
                          <FormItem>
                            <FormLabel>Time Window (minutes)</FormLabel>
                            <FormControl>
                              <Input
                                type="number"
                                {...field}
                                onChange={(e) =>
                                  field.onChange(parseInt(e.target.value))
                                }
                              />
                            </FormControl>
                            <FormMessage />
                          </FormItem>
                        )}
                      />
                    </div>
                  )}
                </FormItem>
              )}
            />
          </div>
        </div>

        <div className="flex justify-end">
          <Button disabled={!form.formState.isDirty} type="submit">
            Save Changes
          </Button>
        </div>
      </form>
    </Form>
  )
}

function DomainAlerts({
  onSave,
  defaultValues,
}: AlertComponentProps<DomainAlertsFormData>) {
  const form = useForm<DomainAlertsFormData>({
    resolver: zodResolver(domainAlertsSchema),
    defaultValues: {
      sslExpirationWarning: {
        enabled: defaultValues?.sslExpirationWarning?.enabled,
        daysBeforeExpiration:
          defaultValues?.sslExpirationWarning?.daysBeforeExpiration,
      },
      domainExpirationWarning: defaultValues?.domainExpirationWarning,
      dnsConfigurationChanges: defaultValues?.dnsConfigurationChanges,
    },
  })

  const handleSubmit = async (data: DomainAlertsFormData) => {
    await onSave(data)
    form.reset(data)
  }

  return (
    <Form {...form}>
      <form onSubmit={form.handleSubmit(handleSubmit)} className="space-y-6">
        <div className="space-y-4">
          <h3 className="text-lg font-medium">Domain Monitoring</h3>
          <div className="space-y-4">
            <FormField
              control={form.control}
              name="sslExpirationWarning.enabled"
              render={({ field }) => (
                <FormItem className="space-y-4">
                  <div className="flex items-center justify-between">
                    <FormLabel>SSL Certificate Expiration</FormLabel>
                    <FormControl>
                      <Switch
                        checked={field.value}
                        onCheckedChange={field.onChange}
                      />
                    </FormControl>
                  </div>

                  {field.value && (
                    <div className="grid gap-2 pl-6">
                      <FormField
                        control={form.control}
                        name="sslExpirationWarning.daysBeforeExpiration"
                        render={({ field }) => (
                          <FormItem>
                            <FormLabel>Days Before Expiration</FormLabel>
                            <FormControl>
                              <Input
                                type="number"
                                {...field}
                                onChange={(e) =>
                                  field.onChange(parseInt(e.target.value))
                                }
                              />
                            </FormControl>
                            <FormMessage />
                          </FormItem>
                        )}
                      />
                    </div>
                  )}
                </FormItem>
              )}
            />

            <FormField
              control={form.control}
              name="domainExpirationWarning"
              render={({ field }) => (
                <FormItem className="flex items-center justify-between">
                  <FormLabel>Domain Expiration Warning</FormLabel>
                  <FormControl>
                    <Switch
                      checked={field.value}
                      onCheckedChange={field.onChange}
                    />
                  </FormControl>
                </FormItem>
              )}
            />

            <FormField
              control={form.control}
              name="dnsConfigurationChanges"
              render={({ field }) => (
                <FormItem className="flex items-center justify-between">
                  <FormLabel>DNS Configuration Changes</FormLabel>
                  <FormControl>
                    <Switch
                      checked={field.value}
                      onCheckedChange={field.onChange}
                    />
                  </FormControl>
                </FormItem>
              )}
            />
          </div>
        </div>

        <div className="flex justify-end">
          <Button disabled={!form.formState.isDirty} type="submit">
            Save Changes
          </Button>
        </div>
      </form>
    </Form>
  )
}

function BackupAlerts({
  onSave,
  defaultValues,
}: AlertComponentProps<BackupAlertsFormData>) {
  const form = useForm<BackupAlertsFormData>({
    resolver: zodResolver(backupAlertsSchema),
    defaultValues: {
      backupFailure: defaultValues?.backupFailure,
      s3ConnectionIssues: defaultValues?.s3ConnectionIssues,
      retentionViolations: defaultValues?.retentionViolations,
      backupSuccess: defaultValues?.backupSuccess,
    },
  })

  const handleSubmit = async (data: BackupAlertsFormData) => {
    await onSave(data)
    form.reset(data)
  }

  return (
    <Form {...form}>
      <form onSubmit={form.handleSubmit(handleSubmit)} className="space-y-6">
        <div className="space-y-4">
          <h3 className="text-lg font-medium">Backup Monitoring</h3>
          <div className="space-y-4">
            <FormField
              control={form.control}
              name="backupSuccess"
              render={({ field }) => (
                <FormItem className="flex items-center justify-between">
                  <div>
                    <FormLabel>Backup Success</FormLabel>
                    <FormDescription>
                      Get notified when backups complete successfully
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
            <FormField
              control={form.control}
              name="backupFailure"
              render={({ field }) => (
                <FormItem className="flex items-center justify-between">
                  <FormLabel>Backup Failures</FormLabel>
                  <FormControl>
                    <Switch
                      checked={field.value}
                      onCheckedChange={field.onChange}
                    />
                  </FormControl>
                </FormItem>
              )}
            />

            <FormField
              control={form.control}
              name="s3ConnectionIssues"
              render={({ field }) => (
                <FormItem className="flex items-center justify-between">
                  <FormLabel>S3 Connection Issues</FormLabel>
                  <FormControl>
                    <Switch
                      checked={field.value}
                      onCheckedChange={field.onChange}
                    />
                  </FormControl>
                </FormItem>
              )}
            />

            <FormField
              control={form.control}
              name="retentionViolations"
              render={({ field }) => (
                <FormItem className="flex items-center justify-between">
                  <FormLabel>Retention Policy Violations</FormLabel>
                  <FormControl>
                    <Switch
                      checked={field.value}
                      onCheckedChange={field.onChange}
                    />
                  </FormControl>
                </FormItem>
              )}
            />
          </div>
        </div>

        <div className="flex justify-end">
          <Button disabled={!form.formState.isDirty} type="submit">
            Save Changes
          </Button>
        </div>
      </form>
    </Form>
  )
}

function RouteAlerts({
  onSave,
  defaultValues,
}: AlertComponentProps<RouteAlertsFormData>) {
  const form = useForm<RouteAlertsFormData>({
    resolver: zodResolver(routeAlertsSchema),
    defaultValues: {
      routeDowntime: defaultValues?.routeDowntime,
      loadBalancerIssues: defaultValues?.loadBalancerIssues,
    },
  })

  const handleSubmit = async (data: RouteAlertsFormData) => {
    await onSave(data)
    form.reset(data)
  }

  return (
    <Form {...form}>
      <form onSubmit={form.handleSubmit(handleSubmit)} className="space-y-6">
        <div className="space-y-4">
          <h3 className="text-lg font-medium">Route Monitoring</h3>
          <div className="space-y-4">
            <FormField
              control={form.control}
              name="routeDowntime"
              render={({ field }) => (
                <FormItem className="flex items-center justify-between">
                  <FormLabel>Route Downtime</FormLabel>
                  <FormControl>
                    <Switch
                      checked={field.value}
                      onCheckedChange={field.onChange}
                    />
                  </FormControl>
                </FormItem>
              )}
            />

            <FormField
              control={form.control}
              name="loadBalancerIssues"
              render={({ field }) => (
                <FormItem className="flex items-center justify-between">
                  <FormLabel>Load Balancer Issues</FormLabel>
                  <FormControl>
                    <Switch
                      checked={field.value}
                      onCheckedChange={field.onChange}
                    />
                  </FormControl>
                </FormItem>
              )}
            />
          </div>
        </div>

        <div className="flex justify-end">
          <Button disabled={!form.formState.isDirty} type="submit">
            Save Changes
          </Button>
        </div>
      </form>
    </Form>
  )
}

function NotificationSettings({
  onSave,
  defaultValues,
}: AlertComponentProps<NotificationSettingsFormData>) {
  const form = useForm<NotificationSettingsFormData>({
    resolver: zodResolver(notificationSettingsSchema),
    defaultValues: {
      email: defaultValues?.email,
      slack: {
        enabled: defaultValues?.slack?.enabled,
        webhook: '',
      },
      batchNotifications: defaultValues?.batchNotifications,
      minimumSeverity: defaultValues?.minimumSeverity,
    },
  })

  const handleSubmit = async (data: NotificationSettingsFormData) => {
    await onSave(data)
    form.reset(data)
  }

  return (
    <Form {...form}>
      <form onSubmit={form.handleSubmit(handleSubmit)} className="space-y-6">
        <div className="space-y-4">
          <h3 className="text-lg font-medium">Notification Preferences</h3>
          <div className="space-y-4">
            <FormField
              control={form.control}
              name="email"
              render={({ field }) => (
                <FormItem className="flex items-center justify-between">
                  <FormLabel>Email Notifications</FormLabel>
                  <FormControl>
                    <Switch
                      checked={field.value}
                      onCheckedChange={field.onChange}
                    />
                  </FormControl>
                </FormItem>
              )}
            />

            <FormField
              control={form.control}
              name="slack.enabled"
              render={({ field }) => (
                <FormItem className="space-y-4">
                  <div className="flex items-center justify-between">
                    <FormLabel>Slack Notifications</FormLabel>
                    <FormControl>
                      <Switch
                        checked={field.value}
                        onCheckedChange={field.onChange}
                      />
                    </FormControl>
                  </div>

                  {field.value && (
                    <div className="grid gap-2 pl-6">
                      <FormField
                        control={form.control}
                        name="slack.webhook"
                        render={({ field }) => (
                          <FormItem>
                            <FormLabel>Webhook URL</FormLabel>
                            <FormControl>
                              <Input
                                {...field}
                                placeholder="https://hooks.slack.com/..."
                              />
                            </FormControl>
                            <FormMessage />
                          </FormItem>
                        )}
                      />
                    </div>
                  )}
                </FormItem>
              )}
            />

            <FormField
              control={form.control}
              name="batchNotifications"
              render={({ field }) => (
                <FormItem className="flex items-center justify-between">
                  <div>
                    <FormLabel>Batch Similar Notifications</FormLabel>
                    <FormDescription>
                      Group similar alerts to reduce noise
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

            <FormField
              control={form.control}
              name="minimumSeverity"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Minimum Alert Severity</FormLabel>
                  <Select
                    onValueChange={field.onChange}
                    defaultValue={field.value}
                  >
                    <FormControl>
                      <SelectTrigger>
                        <SelectValue placeholder="Select minimum severity" />
                      </SelectTrigger>
                    </FormControl>
                    <SelectContent>
                      <SelectItem value="critical">Critical Only</SelectItem>
                      <SelectItem value="warning">
                        Warning & Critical
                      </SelectItem>
                      <SelectItem value="info">All Notifications</SelectItem>
                    </SelectContent>
                  </Select>
                  <FormDescription>
                    Only receive alerts at or above this severity level
                  </FormDescription>
                </FormItem>
              )}
            />
          </div>
        </div>

        <div className="flex justify-end">
          <Button disabled={!form.formState.isDirty} type="submit">
            Save Changes
          </Button>
        </div>
      </form>
    </Form>
  )
}

export function MonitoringSettings() {
  const navigate = useNavigate()
  const { section } = useParams()
  const currentSection = section || 'project'

  const { data: preferences, isLoading } = useQuery({
    queryKey: ['preferences'],
    queryFn: async () => {
      const { data } = await getPreferences()
      return data
    },
  })

  const handleSectionChange = (value: string) => {
    navigate(`/monitoring/${value}`)
  }

  const settingsSections = [
    { id: 'project', label: 'Project Health' },
    { id: 'domains', label: 'Domains' },
    { id: 'backups', label: 'Backups' },
    { id: 'routes', label: 'Routes' },
    { id: 'notifications', label: 'Notifications' },
    { id: 'providers', label: 'Providers' },
  ] as const

  const handleProjectSave = async (data: ProjectAlertsFormData) => {
    if (!preferences) return

    const updatedPreferences: NotificationPreferencesResponse = {
      ...preferences,
      deployment_failures_enabled: data.deploymentFailures,
      build_errors_enabled: data.buildErrors,
      runtime_errors_enabled: data.runtimeErrors.enabled,
      error_threshold: data.runtimeErrors.errorThreshold,
      error_time_window: data.runtimeErrors.windowMinutes,
    }

    await toast.promise(
      updatePreferences({
        body: {
          preferences: updatedPreferences,
        },
      }),
      {
        loading: 'Saving project alert settings...',
        success: 'Project alert settings saved successfully',
        error: 'Failed to save project alert settings',
      }
    )
  }

  const handleDomainSave = async (data: DomainAlertsFormData) => {
    if (!preferences) return

    const updatedPreferences: NotificationPreferencesResponse = {
      ...preferences,
      ssl_expiration_enabled: data.sslExpirationWarning.enabled,
      ssl_days_before_expiration:
        data.sslExpirationWarning.daysBeforeExpiration,
      domain_expiration_enabled: data.domainExpirationWarning,
      dns_changes_enabled: data.dnsConfigurationChanges,
    }

    await toast.promise(
      updatePreferences({
        body: {
          preferences: updatedPreferences,
        },
      }),
      {
        loading: 'Saving domain alert settings...',
        success: 'Domain alert settings saved successfully',
        error: 'Failed to save domain alert settings',
      }
    )
  }

  const handleBackupSave = async (data: BackupAlertsFormData) => {
    if (!preferences) return

    const updatedPreferences: NotificationPreferencesResponse = {
      ...preferences,
      backup_failures_enabled: data.backupFailure,
      s3_connection_issues_enabled: data.s3ConnectionIssues,
      retention_policy_violations_enabled: data.retentionViolations,
      backup_successes_enabled: data.backupSuccess,
    }

    await toast.promise(
      updatePreferences({
        body: {
          preferences: updatedPreferences,
        },
      }),
      {
        loading: 'Saving backup alert settings...',
        success: 'Backup alert settings saved successfully',
        error: 'Failed to save backup alert settings',
      }
    )
  }

  const handleRouteSave = async (data: RouteAlertsFormData) => {
    if (!preferences) return

    const updatedPreferences: NotificationPreferencesResponse = {
      ...preferences,
      route_downtime_enabled: data.routeDowntime,
      load_balancer_issues_enabled: data.loadBalancerIssues,
    }

    await toast.promise(
      updatePreferences({
        body: {
          preferences: updatedPreferences,
        },
      }),
      {
        loading: 'Saving route alert settings...',
        success: 'Route alert settings saved successfully',
        error: 'Failed to save route alert settings',
      }
    )
  }

  const handleNotificationSave = async (data: NotificationSettingsFormData) => {
    if (!preferences) return

    const updatedPreferences: NotificationPreferencesResponse = {
      ...preferences,
      email_enabled: data.email,
      slack_enabled: data.slack.enabled,
      batch_similar_notifications: data.batchNotifications,
      minimum_severity: data.minimumSeverity,
    }

    await toast.promise(
      updatePreferences({
        body: {
          preferences: updatedPreferences,
        },
      }),
      {
        loading: 'Saving notification settings...',
        success: 'Notification settings saved successfully',
        error: 'Failed to save notification settings',
      }
    )
  }

  const renderContent = () => {
    if (isLoading) {
      return (
        <div className="flex items-center justify-center py-6">
          <div className="animate-spin rounded-full h-8 w-8 border-b-2 border-primary"></div>
        </div>
      )
    }

    if (!preferences) {
      return (
        <div className="text-center py-6 text-muted-foreground">
          Failed to load preferences
        </div>
      )
    }

    const projectDefaults = {
      deploymentFailures: preferences.deployment_failures_enabled,
      buildErrors: preferences.build_errors_enabled,
      runtimeErrors: {
        enabled: preferences.runtime_errors_enabled,
        errorThreshold: preferences.error_threshold,
        windowMinutes: preferences.error_time_window,
      },
    }

    const domainDefaults = {
      sslExpirationWarning: {
        enabled: preferences.ssl_expiration_enabled,
        daysBeforeExpiration: preferences.ssl_days_before_expiration,
      },
      domainExpirationWarning: preferences.domain_expiration_enabled,
      dnsConfigurationChanges: preferences.dns_changes_enabled,
    }

    const backupDefaults = {
      backupFailure: preferences.backup_failures_enabled,
      s3ConnectionIssues: preferences.s3_connection_issues_enabled,
      retentionViolations: preferences.retention_policy_violations_enabled,
      backupSuccess: preferences.backup_successes_enabled,
    }

    const routeDefaults = {
      routeDowntime: preferences.route_downtime_enabled,
      loadBalancerIssues: preferences.load_balancer_issues_enabled,
    }

    const notificationDefaults = {
      email: preferences.email_enabled,
      slack: {
        enabled: preferences.slack_enabled,
        webhook: '',
      },
      batchNotifications: preferences.batch_similar_notifications,
      minimumSeverity: preferences.minimum_severity as
        | 'critical'
        | 'warning'
        | 'info',
    }

    switch (currentSection) {
      case 'project':
        return (
          <ProjectAlerts
            onSave={handleProjectSave}
            defaultValues={projectDefaults}
          />
        )
      case 'domains':
        return (
          <DomainAlerts
            onSave={handleDomainSave}
            defaultValues={domainDefaults}
          />
        )
      case 'backups':
        return (
          <BackupAlerts
            onSave={handleBackupSave}
            defaultValues={backupDefaults}
          />
        )
      case 'routes':
        return (
          <RouteAlerts onSave={handleRouteSave} defaultValues={routeDefaults} />
        )
      case 'notifications':
        return (
          <NotificationSettings
            onSave={handleNotificationSave}
            defaultValues={notificationDefaults}
          />
        )
      case 'providers':
        return <ProvidersManagement />
      default:
        return null
    }
  }

  return (
    <div className="space-y-6">
      <div>
        <h2 className="text-lg font-semibold">Monitoring & Alerts</h2>
        <p className="text-sm text-muted-foreground">
          Configure monitoring thresholds and alert notifications
        </p>
      </div>

      {/* Mobile Select */}
      <div className="sm:hidden">
        <Select value={currentSection} onValueChange={handleSectionChange}>
          <SelectTrigger className="w-full">
            <SelectValue>
              {settingsSections.find((section) => section.id === currentSection)
                ?.label || 'Select section'}
            </SelectValue>
          </SelectTrigger>
          <SelectContent>
            {settingsSections.map((section) => (
              <SelectItem key={section.id} value={section.id}>
                {section.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>

      {/* Desktop Tabs */}
      <div className="hidden sm:block">
        <Tabs
          value={currentSection}
          onValueChange={handleSectionChange}
          className="space-y-4"
        >
          <TabsList>
            {settingsSections.map((section) => (
              <TabsTrigger key={section.id} value={section.id}>
                {section.label}
              </TabsTrigger>
            ))}
          </TabsList>
        </Tabs>
      </div>

      {/* Content - Shared between mobile and desktop */}
      <Card className="p-6">{renderContent()}</Card>
    </div>
  )
}
