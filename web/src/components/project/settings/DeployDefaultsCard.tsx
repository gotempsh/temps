// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ProjectResponse } from '@/api/client'
import { updateProjectDeploymentConfigMutation } from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardFooter,
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
import { BUILD_LOCATION_LABELS } from '@/lib/build-location'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation } from '@tanstack/react-query'
import { Loader2 } from 'lucide-react'
import { useState, type FormEvent, type ReactNode } from 'react'
import { useForm, useFormState, useWatch } from 'react-hook-form'
import { toast } from 'sonner'
import { z } from 'zod'
import {
  configuredProbeType,
  healthProbeFormSchema,
  healthConfigFromForm,
} from './health-probe'

const schema = z
  .object({
    cpuRequest: z.string().optional(),
    cpuLimit: z.string().optional(),
    memoryRequest: z.string().optional(),
    memoryLimit: z.string().optional(),
    replicas: z.string().optional(),
    port: z.string().optional(),
    healthCheckTimeout: z
      .string()
      .optional()
      .refine(
        (value) =>
          !value ||
          value.trim() === '' ||
          (Number.isInteger(Number(value)) &&
            Number(value) >= 30 &&
            Number(value) <= 3600),
        { message: 'Enter a whole number of seconds between 30 and 3600' }
      ),
    buildLocation: z.enum(['control_plane', 'node', 'ci']),
  })
  .and(healthProbeFormSchema)

const probeExamples = {
  auto: 'HTTP when a port is declared in settings or by the image; process otherwise.',
  http: 'Example: GET /ready for a web API. Requires a declared port.',
  tcp: 'Example: connect to port 6379 for a TCP service. Requires a declared port.',
  process:
    'Example: a Redis subscriber worker stays running without restarting for 15 seconds.',
  exec: 'Example: ["redis-cli", "ping"] exits 0 when the worker dependency is ready.',
}

type FormValues = z.infer<typeof schema>

/**
 * The settings this page saves separately, each from its own card. A card
 * sends only its own fields, so saving one never writes another card's
 * unsaved (or stale) values.
 */
type Group = 'resources' | 'health' | 'build'

const GROUP_FIELDS: Record<Group, (keyof FormValues)[]> = {
  resources: [
    'cpuRequest',
    'cpuLimit',
    'memoryRequest',
    'memoryLimit',
    'replicas',
  ],
  health: [
    'port',
    'probeType',
    'healthPath',
    'stableFor',
    'probeTimeout',
    'probeInterval',
    'probeCommand',
    'healthCheckTimeout',
  ],
  build: ['buildLocation'],
}

const SAVED: Record<Group, string> = {
  resources: 'Resources saved',
  health: 'Port and health check saved',
  build: 'Build location saved',
}

/** Blank input means "no override" — send null rather than 0/NaN. */
function optionalInt(value: string | undefined): number | null {
  return value && value.trim() !== '' ? parseInt(value, 10) : null
}

/** CPU is entered in cores and stored in millionths of a core. */
function optionalCores(value: string | undefined): number | null {
  return value && value.trim() !== ''
    ? Math.round(parseFloat(value) * 1_000_000)
    : null
}

/**
 * Resource, health-check and build defaults for every environment, one setting
 * card each.
 *
 * Submits only the fields it owns. `update_project_deployment_config` applies
 * `if let Some(..)` per field, so the monitoring toggles this card does not
 * render are left untouched rather than reset — which is what makes it safe to
 * split the old single "Default Deployment Configuration" form across tabs.
 */
export function DeployDefaultsCard({
  project,
  refetch,
}: {
  project: ProjectResponse
  refetch: () => void
}) {
  const updateDeploymentConfig = useMutation({
    ...updateProjectDeploymentConfigMutation(),
    meta: { errorTitle: 'Failed to update deployment configuration' },
  })

  const form = useForm<FormValues>({
    resolver: zodResolver(schema),
    defaultValues: {
      cpuRequest:
        project?.deployment_config?.cpuRequest != null
          ? (project.deployment_config.cpuRequest / 1_000_000).toString()
          : '',
      cpuLimit:
        project?.deployment_config?.cpuLimit != null
          ? (project.deployment_config.cpuLimit / 1_000_000).toString()
          : '',
      memoryRequest:
        project?.deployment_config?.memoryRequest?.toString() ?? '',
      memoryLimit: project?.deployment_config?.memoryLimit?.toString() ?? '',
      replicas: project?.deployment_config?.replicas?.toString() ?? '',
      port: project?.deployment_config?.exposedPort?.toString() ?? '',
      healthCheckTimeout:
        project?.deployment_config?.healthCheckTimeoutSeconds?.toString() ?? '',
      probeType: configuredProbeType(project.deployment_config?.health),
      stableFor: project.deployment_config?.health?.stable_for ?? 15,
      probeTimeout: project.deployment_config?.health?.timeout ?? 5,
      probeInterval: project.deployment_config?.health?.interval ?? 30,
      probeCommand: JSON.stringify(
        project.deployment_config?.health?.command ?? []
      ),
      healthPath: project.deployment_config?.health?.path ?? '/',
      buildLocation:
        project?.deployment_config?.buildLocation ?? 'control_plane',
    },
  })

  const probeType = useWatch({ control: form.control, name: 'probeType' })

  const [saving, setSaving] = useState<Group | null>(null)
  const { dirtyFields } = useFormState({ control: form.control })
  const isDirty = (group: Group) =>
    GROUP_FIELDS[group].some((name) => dirtyFields[name])

  const bodyFor = (group: Group, values: FormValues) => {
    switch (group) {
      case 'resources':
        return {
          cpuRequest: optionalCores(values.cpuRequest),
          cpuLimit: optionalCores(values.cpuLimit),
          memoryRequest: optionalInt(values.memoryRequest),
          memoryLimit: optionalInt(values.memoryLimit),
          replicas: optionalInt(values.replicas),
        }
      case 'health':
        return {
          exposedPort: optionalInt(values.port),
          healthCheckTimeoutSeconds:
            optionalInt(values.healthCheckTimeout) ?? 300,
          health: healthConfigFromForm(values),
        }
      case 'build':
        return { buildLocation: values.buildLocation }
    }
  }

  const save = (group: Group) => async (event: FormEvent) => {
    event.preventDefault()
    if (!project?.id) return
    const fields = GROUP_FIELDS[group]
    if (!(await form.trigger(fields))) {
      toast.error('Check the highlighted deployment settings')
      return
    }
    const values = form.getValues()
    setSaving(group)
    try {
      await updateDeploymentConfig.mutateAsync({
        path: { project_id: project.id },
        body: bodyFor(group, values),
      })
    } catch {
      // The mutation's errorTitle already explains the failure.
      return
    } finally {
      setSaving(null)
    }
    for (const name of fields) {
      form.resetField(name, { defaultValue: values[name] })
    }
    toast.success(SAVED[group])
    refetch()
  }

  const footer = (group: Group, hint: ReactNode) => (
    <CardFooter>
      <p>{hint}</p>
      <Button
        type="submit"
        size="sm"
        disabled={!isDirty(group) || saving !== null}
      >
        {saving === group && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
        Save
      </Button>
    </CardFooter>
  )

  return (
    <Form {...form}>
      <form
        id="setting-resource-limits"
        className="scroll-mt-4"
        onSubmit={save('resources')}
      >
        <Card>
          <CardHeader>
            <CardTitle>Resources</CardTitle>
            <CardDescription>
              CPU, memory and replicas every environment starts with.
              Environments can override them.
            </CardDescription>
          </CardHeader>
          <CardContent>
            <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
              <FormField
                control={form.control}
                name="cpuRequest"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>CPU request (cores)</FormLabel>
                    <FormControl>
                      <Input
                        {...field}
                        type="number"
                        step="any"
                        min="0.01"
                        placeholder="e.g., 0.1"
                      />
                    </FormControl>
                    <FormDescription className="text-muted-foreground">
                      Cores reserved for each instance.
                    </FormDescription>
                  </FormItem>
                )}
              />

              <FormField
                control={form.control}
                name="cpuLimit"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>CPU limit (cores)</FormLabel>
                    <FormControl>
                      <Input
                        {...field}
                        type="number"
                        step="any"
                        min="0.01"
                        placeholder="e.g., 1"
                      />
                    </FormControl>
                    <FormDescription className="text-muted-foreground">
                      Most cores an instance may use.
                    </FormDescription>
                  </FormItem>
                )}
              />

              <FormField
                control={form.control}
                name="memoryRequest"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Memory request (MB)</FormLabel>
                    <FormControl>
                      <Input
                        {...field}
                        type="number"
                        min="1"
                        placeholder="e.g., 128"
                      />
                    </FormControl>
                    <FormDescription className="text-muted-foreground">
                      Memory reserved for each instance.
                    </FormDescription>
                  </FormItem>
                )}
              />

              <FormField
                control={form.control}
                name="memoryLimit"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Memory limit (MB)</FormLabel>
                    <FormControl>
                      <Input
                        {...field}
                        type="number"
                        min="0"
                        placeholder="e.g., 256"
                      />
                    </FormControl>
                    <FormDescription className="text-muted-foreground">
                      Maximum memory allocation. Leave empty to use the default,
                      or set <code>0</code> to run uncapped.
                    </FormDescription>
                  </FormItem>
                )}
              />

              <FormField
                control={form.control}
                name="replicas"
                render={({ field }) => (
                  <FormItem
                    id="setting-replicas-and-port"
                    className="scroll-mt-4"
                  >
                    <FormLabel>Replicas</FormLabel>
                    <FormControl>
                      <Input
                        {...field}
                        type="number"
                        min="1"
                        placeholder="e.g., 1"
                      />
                    </FormControl>
                    <FormDescription className="text-muted-foreground">
                      Container instances per environment.
                    </FormDescription>
                  </FormItem>
                )}
              />
            </div>
          </CardContent>
          {footer(
            'resources',
            'Leave a field empty to use the platform default.'
          )}
        </Card>
      </form>

      <form
        id="setting-port-and-health"
        className="scroll-mt-4"
        onSubmit={save('health')}
      >
        <Card>
          <CardHeader>
            <CardTitle>Port and health check</CardTitle>
            <CardDescription>
              Where the app listens, and how Temps decides a new deployment is
              ready before it takes traffic.
            </CardDescription>
          </CardHeader>
          <CardContent>
            <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
              <FormField
                control={form.control}
                name="port"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Port</FormLabel>
                    <FormControl>
                      <Input
                        {...field}
                        type="number"
                        min="1"
                        max="65535"
                        placeholder="e.g., 3000"
                      />
                    </FormControl>
                    <FormDescription className="text-muted-foreground">
                      Leave empty to use the image’s declared port. Images
                      without a port deploy as background workers.
                    </FormDescription>
                  </FormItem>
                )}
              />

              <FormField
                control={form.control}
                name="probeType"
                render={({ field }) => (
                  <FormItem id="health-probe" className="scroll-mt-4">
                    <FormLabel>Health probe</FormLabel>
                    <Select value={field.value} onValueChange={field.onChange}>
                      <FormControl>
                        <SelectTrigger>
                          <SelectValue />
                        </SelectTrigger>
                      </FormControl>
                      <SelectContent>
                        <SelectItem value="auto">Automatic</SelectItem>
                        <SelectItem value="http">HTTP</SelectItem>
                        <SelectItem value="tcp">TCP</SelectItem>
                        <SelectItem value="process">
                          Process (background worker)
                        </SelectItem>
                        <SelectItem value="exec">Exec command</SelectItem>
                      </SelectContent>
                    </Select>
                    <FormDescription>
                      {probeExamples[field.value]}
                    </FormDescription>
                    <FormMessage />
                  </FormItem>
                )}
              />
              {probeType === 'http' && (
                <FormField
                  control={form.control}
                  name="healthPath"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>HTTP health path</FormLabel>
                      <FormControl>
                        <Input {...field} placeholder="/ready" />
                      </FormControl>
                      <FormMessage />
                    </FormItem>
                  )}
                />
              )}
              {(probeType === 'process' || probeType === 'auto') && (
                <FormField
                  control={form.control}
                  name="stableFor"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Process stability window (seconds)</FormLabel>
                      <FormControl>
                        <Input
                          {...field}
                          onChange={(event) =>
                            field.onChange(event.target.valueAsNumber)
                          }
                          type="number"
                          min="1"
                          max="3600"
                        />
                      </FormControl>
                      <FormDescription>
                        The worker must stay running without a restart for this
                        window. The startup timeout still applies.
                      </FormDescription>
                      <FormMessage />
                    </FormItem>
                  )}
                />
              )}
              {probeType === 'exec' && (
                <FormField
                  control={form.control}
                  name="probeCommand"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Health command (JSON arguments)</FormLabel>
                      <FormControl>
                        <Input {...field} placeholder='["redis-cli", "ping"]' />
                      </FormControl>
                      <FormDescription>
                        Runs inside the container, without an implicit shell.
                        Exit 0 passes the gate.
                      </FormDescription>
                      <FormMessage />
                    </FormItem>
                  )}
                />
              )}
              {(probeType === 'exec' || probeType === 'tcp') && (
                <>
                  <FormField
                    control={form.control}
                    name="probeTimeout"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Probe timeout (seconds)</FormLabel>
                        <FormControl>
                          <Input
                            {...field}
                            onChange={(event) =>
                              field.onChange(event.target.valueAsNumber)
                            }
                            type="number"
                            min="1"
                            max="300"
                          />
                        </FormControl>
                        <FormMessage />
                      </FormItem>
                    )}
                  />
                  <FormField
                    control={form.control}
                    name="probeInterval"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Probe interval (seconds)</FormLabel>
                        <FormControl>
                          <Input
                            {...field}
                            onChange={(event) =>
                              field.onChange(event.target.valueAsNumber)
                            }
                            type="number"
                            min="1"
                            max="3600"
                          />
                        </FormControl>
                        <FormMessage />
                      </FormItem>
                    )}
                  />
                </>
              )}
              <FormField
                control={form.control}
                name="healthCheckTimeout"
                render={({ field }) => (
                  <FormItem id="startup-timeout">
                    <FormLabel>Startup timeout (seconds)</FormLabel>
                    <FormControl>
                      <Input
                        {...field}
                        type="number"
                        min="30"
                        max="3600"
                        placeholder="300"
                      />
                    </FormControl>
                    <FormDescription className="text-muted-foreground">
                      How long a new deployment may take to start and pass its
                      health check before it fails. Raise it for apps that run
                      migrations or warm caches on boot. Default 300.
                    </FormDescription>
                    <FormMessage />
                  </FormItem>
                )}
              />
            </div>
          </CardContent>
          {footer(
            'health',
            'Repository health settings override these. Compose keeps its own service healthchecks.'
          )}
        </Card>
      </form>

      <form
        id="setting-build-location"
        className="scroll-mt-4"
        onSubmit={save('build')}
      >
        <Card>
          <CardHeader>
            <CardTitle>Build location</CardTitle>
            <CardDescription>
              Where images are built. Environments can override this.
            </CardDescription>
          </CardHeader>
          <CardContent>
            <FormField
              control={form.control}
              name="buildLocation"
              render={({ field }) => (
                <FormItem>
                  <FormLabel className="sr-only">Build location</FormLabel>
                  <Select value={field.value} onValueChange={field.onChange}>
                    <FormControl>
                      <SelectTrigger className="w-full sm:w-[240px]">
                        <SelectValue />
                      </SelectTrigger>
                    </FormControl>
                    <SelectContent>
                      <SelectItem value="control_plane">
                        {BUILD_LOCATION_LABELS.control_plane}
                      </SelectItem>
                      <SelectItem value="node">
                        {BUILD_LOCATION_LABELS.node}
                      </SelectItem>
                      <SelectItem value="ci">
                        {BUILD_LOCATION_LABELS.ci}
                      </SelectItem>
                    </SelectContent>
                  </Select>
                </FormItem>
              )}
            />
          </CardContent>
          {footer(
            'build',
            <>
              A worker keeps build CPU and memory away from the proxy and
              running apps. With no worker free, the build runs on the control
              plane when local builds are enabled, and its log says why. CI
              builds use the GitHub Actions runner:{' '}
              <a href="/settings/build-limits#ci-builder" className="underline">
                configure the CI builder
              </a>
              .
            </>
          )}
        </Card>
      </form>
    </Form>
  )
}
