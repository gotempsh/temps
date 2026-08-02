import {
  type CustomDomainResponse,
  type DomainEnvironmentResponse,
} from '@/api/client'
import {
  createCustomDomainMutation,
  listContainersOptions,
  listDomainsOptions,
  updateCustomDomainMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import { DomainSelector } from '@/components/domains/DomainSelector'
import {
  Form,
  FormControl,
  FormField,
  FormItem,
  FormLabel,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { useSettings } from '@/hooks/useSettings'
import { reservedHostnameReason } from '@/lib/reservedHostnames'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery } from '@tanstack/react-query'
import { X } from 'lucide-react'
import { useMemo, useState } from 'react'
import { useForm, useWatch } from 'react-hook-form'
import { toast } from 'sonner'
import { z } from 'zod'

const domainFormSchema = z.object({
  domain: z.string().min(1, 'Domain is required'),
  environment: z.string().min(1, 'Environment is required'),
  redirectTo: z.string().optional(),
  statusCode: z.number().optional(),
  serviceName: z.string().optional(),
})

type DomainFormValues = z.infer<typeof domainFormSchema>

interface DomainFormProps {
  project_id: number
  environments: DomainEnvironmentResponse[]
  onSuccess: () => void
  onCancel: () => void
  initialData?: CustomDomainResponse
  /** Project preset — when docker-compose, shows service selector */
  preset?: string | null
}

/**
 * Given a full domain (e.g. "app.example.com") and a list of wildcard domains,
 * find the matching wildcard and extract the subdomain part.
 */
function matchWildcardDomain(
  fullDomain: string,
  wildcardDomains: { domain: string }[]
): { subdomain: string; selectedDomain: string } {
  for (const wd of wildcardDomains) {
    const wildcardPattern = wd.domain.replace('*', '(.+)')
    const regex = new RegExp(`^${wildcardPattern}$`)
    if (regex.test(fullDomain)) {
      const wildcardBase = wd.domain.split('*.')?.[1]
      const subdomain = fullDomain.split(`.${wildcardBase}`)?.[0]
      return { subdomain: subdomain ?? '', selectedDomain: wd.domain }
    }
  }
  return { subdomain: '', selectedDomain: fullDomain }
}

export function DomainForm({
  project_id,
  environments,
  onSuccess,
  onCancel,
  initialData,
  preset,
}: DomainFormProps) {
  const isDockerCompose = preset === 'docker-compose' || preset === 'dockercompose'
  // Fetch wildcard domains for initial state matching when editing
  const { data: wildcardData } = useQuery({
    ...listDomainsOptions({
      query: { search: '*.', page_size: 100 },
    }),
    enabled: !!initialData,
  })

  const wildcardDomains = useMemo(
    () => wildcardData?.domains?.filter((d) => d.domain.startsWith('*.')) ?? [],
    [wildcardData]
  )

  const { subdomain: initialSubdomain, selectedDomain: initialSelectedDomain } =
    useMemo(() => {
      if (!initialData?.domain) return { subdomain: '', selectedDomain: '' }
      return matchWildcardDomain(initialData.domain, wildcardDomains)
    }, [initialData?.domain, wildcardDomains])

  const {
    subdomain: initialRedirectSubdomain,
    selectedDomain: initialSelectedRedirectDomain,
  } = useMemo(() => {
    if (!initialData?.redirect_to) return { subdomain: '', selectedDomain: '' }
    return matchWildcardDomain(initialData.redirect_to, wildcardDomains)
  }, [initialData?.redirect_to, wildcardDomains])

  const [subdomain, setSubdomain] = useState(initialSubdomain)
  const [selectedDomain, setSelectedDomain] = useState(initialSelectedDomain)
  const [redirectSubdomain, setRedirectSubdomain] = useState(
    initialRedirectSubdomain
  )
  const [selectedRedirectDomain, setSelectedRedirectDomain] = useState(
    initialSelectedRedirectDomain
  )

  const createDomain = useMutation({
    ...createCustomDomainMutation(),
    meta: {
      errorTitle: 'Failed to add domain',
    },
    onSuccess: () => {
      toast.success('Domain added successfully')
      onSuccess()
    },
  })

  const updateDomain = useMutation({
    ...updateCustomDomainMutation(),
    meta: {
      errorTitle: 'Failed to update domain',
    },
    onSuccess: () => {
      toast.success('Domain updated successfully')
      onSuccess()
    },
  })

  const form = useForm<DomainFormValues>({
    resolver: zodResolver(domainFormSchema),
    defaultValues: {
      domain: initialData?.domain ?? '',
      environment:
        (
          initialData?.environment as unknown as DomainEnvironmentResponse
        )?.id.toString() ??
        environments?.[0]?.id.toString() ??
        '',
      redirectTo: initialData?.redirect_to ?? '',
      statusCode: initialData?.status_code ?? 301,
      serviceName: (initialData as any)?.service_name ?? '',
    },
  })

  const onSubmit = (data: DomainFormValues) => {
    const payload = {
      domain: data.domain,
      environment_id: parseInt(data.environment),
      redirect_to: data.redirectTo || undefined,
      status_code: data.redirectTo ? data.statusCode : undefined,
      service_name: data.serviceName || undefined,
    }

    if (initialData) {
      updateDomain.mutate({
        path: {
          project_id,
          domain_id: initialData.id,
        },
        body: payload,
      })
    } else {
      createDomain.mutate({
        path: {
          project_id,
        },
        body: payload,
      })
    }
  }
  // Watch environment to fetch compose service names
  const watchedEnvironment = useWatch({
    control: form.control,
    name: 'environment',
  })

  // Fetch containers for the selected environment to get compose service names
  const { data: containersData } = useQuery({
    ...listContainersOptions({
      path: {
        project_id,
        environment_id: parseInt(watchedEnvironment || '0'),
      },
    }),
    enabled: isDockerCompose && !!watchedEnvironment && parseInt(watchedEnvironment) > 0,
  })

  // Extract unique service names from containers
  const serviceNames = useMemo(() => {
    if (!containersData?.containers) return []
    const names = containersData.containers
      .map((c) => c.service_name)
      .filter((n): n is string => !!n)
    return [...new Set(names)].sort()
  }, [containersData])

  const watchedRedirectTo = useWatch({
    control: form.control,
    name: 'redirectTo',
  })

  // Reserved hostnames (console, preview apex) would take the control plane
  // offline if routed at a project — block them before submit (issue #478).
  const { data: platformSettings } = useSettings()
  const watchedDomain = useWatch({ control: form.control, name: 'domain' })
  const reservedDomainReason = useMemo(
    () => reservedHostnameReason(watchedDomain ?? '', platformSettings),
    [watchedDomain, platformSettings]
  )
  return (
    <Form {...form}>
      <form onSubmit={form.handleSubmit(onSubmit)} className="space-y-4">
        <FormField
          control={form.control}
          name="domain"
          render={({ field }) => (
            <FormItem>
              <FormLabel>Domain</FormLabel>
              <div className="flex flex-col gap-2">
                <FormControl>
                  <DomainSelector
                    value={selectedDomain || field.value}
                    onValueChange={(value) => {
                      setSubdomain('')
                      setSelectedDomain(value)
                      field.onChange(value)
                    }}
                    placeholder="Select domain"
                    className="w-full"
                  />
                </FormControl>

                {selectedDomain.includes('*') && (
                  <div className="flex items-center gap-2">
                    <FormControl>
                      <Input
                        placeholder="Enter subdomain (e.g. app1)"
                        value={subdomain}
                        onChange={(e) => {
                          const newSubdomain = e.target.value
                          setSubdomain(newSubdomain)
                          const fullDomain = selectedDomain.replace(
                            '*',
                            newSubdomain
                          )
                          field.onChange(fullDomain)
                        }}
                        className="flex-1"
                      />
                    </FormControl>
                    <span className="text-sm text-muted-foreground whitespace-nowrap">
                      .{selectedDomain.split('*.')?.[1]}
                    </span>
                  </div>
                )}

                {reservedDomainReason && (
                  <p className="text-sm text-destructive">
                    {reservedDomainReason}
                  </p>
                )}
              </div>
            </FormItem>
          )}
        />

        <FormField
          control={form.control}
          name="environment"
          render={({ field }) => (
            <FormItem>
              <FormLabel>Environment</FormLabel>
              <Select onValueChange={field.onChange} defaultValue={field.value}>
                <FormControl>
                  <SelectTrigger>
                    <SelectValue placeholder="Select environment" />
                  </SelectTrigger>
                </FormControl>
                <SelectContent>
                  {environments?.map((env) => (
                    <SelectItem key={env.id} value={env.id.toString()}>
                      {env.name}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </FormItem>
          )}
        />

        {isDockerCompose && (
          <FormField
            control={form.control}
            name="serviceName"
            render={({ field }) => (
              <FormItem>
                <FormLabel>Compose Service</FormLabel>
                <Select
                  onValueChange={(val) => field.onChange(val === '_all_' ? '' : val)}
                  value={field.value || '_all_'}
                >
                  <FormControl>
                    <SelectTrigger>
                      <SelectValue placeholder="All services" />
                    </SelectTrigger>
                  </FormControl>
                  <SelectContent>
                    <SelectItem value="_all_">All services</SelectItem>
                    {serviceNames.map((name) => (
                      <SelectItem key={name} value={name}>
                        {name}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
                <p className="text-xs text-muted-foreground">
                  Route this domain to a specific Docker Compose service
                </p>
              </FormItem>
            )}
          />
        )}

        <FormField
          control={form.control}
          name="redirectTo"
          render={({ field }) => (
            <FormItem>
              <div className="flex items-center justify-between">
                <FormLabel>Redirect to (optional)</FormLabel>
                {(selectedRedirectDomain &&
                  selectedRedirectDomain !== '_none_') ||
                (field.value && field.value !== '_none_') ? (
                  <Button
                    type="button"
                    variant="ghost"
                    size="sm"
                    onClick={() => {
                      setRedirectSubdomain('')
                      setSelectedRedirectDomain('_none_')
                      field.onChange('')
                    }}
                    className="h-auto p-1 text-muted-foreground hover:text-foreground"
                  >
                    <X className="h-4 w-4" />
                    <span className="ml-1 text-xs">Clear</span>
                  </Button>
                ) : null}
              </div>
              <div className="flex flex-col gap-2">
                <FormControl>
                  <DomainSelector
                    value={selectedRedirectDomain || field.value || ''}
                    onValueChange={(value) => {
                      setRedirectSubdomain('')
                      setSelectedRedirectDomain(value)
                      field.onChange(value)
                    }}
                    placeholder="No redirect"
                    className="w-full"
                  />
                </FormControl>

                {selectedRedirectDomain?.includes('*') && (
                  <div className="flex items-center gap-2">
                    <FormControl>
                      <Input
                        placeholder="Enter subdomain (e.g. app1)"
                        value={redirectSubdomain}
                        onChange={(e) => {
                          const newSubdomain = e.target.value
                          setRedirectSubdomain(newSubdomain)
                          const fullDomain = selectedRedirectDomain.replace(
                            '*',
                            newSubdomain
                          )
                          field.onChange(fullDomain)
                        }}
                        className="flex-1"
                      />
                    </FormControl>
                    <span className="text-sm text-muted-foreground whitespace-nowrap">
                      .{selectedRedirectDomain.split('*.')?.[1]}
                    </span>
                  </div>
                )}
              </div>
            </FormItem>
          )}
        />

        {watchedRedirectTo && (
          <FormField
            control={form.control}
            name="statusCode"
            render={({ field }) => (
              <FormItem>
                <FormLabel>Redirect Status Code</FormLabel>
                <Select
                  onValueChange={(value) => field.onChange(parseInt(value))}
                  defaultValue={field.value?.toString()}
                >
                  <FormControl>
                    <SelectTrigger>
                      <SelectValue placeholder="Select status code" />
                    </SelectTrigger>
                  </FormControl>
                  <SelectContent>
                    <SelectItem value="301">
                      301 - Permanent Redirect
                    </SelectItem>
                    <SelectItem value="302">
                      302 - Temporary Redirect
                    </SelectItem>
                  </SelectContent>
                </Select>
              </FormItem>
            )}
          />
        )}

        <div className="flex justify-end gap-2 pt-4">
          <Button variant="outline" type="button" onClick={onCancel}>
            Cancel
          </Button>
          <Button
            type="submit"
            disabled={
              createDomain.isPending ||
              updateDomain.isPending ||
              !!reservedDomainReason
            }
          >
            {createDomain.isPending || updateDomain.isPending
              ? initialData
                ? 'Updating...'
                : 'Adding...'
              : initialData
                ? 'Update'
                : 'Add'}
          </Button>
        </div>
      </form>
    </Form>
  )
}
