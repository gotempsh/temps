import {
  type CustomDomainResponse,
  type DomainEnvironmentResponse,
} from '@/api/client'
import {
  createCustomDomainMutation,
  updateCustomDomainMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
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
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation } from '@tanstack/react-query'
import { X } from 'lucide-react'
import { useState } from 'react'
import { useForm, useWatch } from 'react-hook-form'
import { toast } from 'sonner'
import { z } from 'zod'

const domainFormSchema = z.object({
  domain: z.string().min(1, 'Domain is required'),
  environment: z.string().min(1, 'Environment is required'),
  redirectTo: z.string().optional(),
  statusCode: z.number().optional(),
})

type DomainFormValues = z.infer<typeof domainFormSchema>

interface DomainFormProps {
  project_id: number
  environments: DomainEnvironmentResponse[]
  domains: { id: string; domain: string }[]
  onSuccess: () => void
  onCancel: () => void
  initialData?: CustomDomainResponse
}

export function DomainForm({
  project_id,
  environments,
  domains,
  onSuccess,
  onCancel,
  initialData,
}: DomainFormProps) {
  const getInitialDomainState = () => {
    if (!initialData?.domain) return { subdomain: '', selectedDomain: '' }

    const wildcardDomain = domains.find((d) => {
      const wildcardPattern = d.domain.replace('*', '(.+)')
      const regex = new RegExp(`^${wildcardPattern}$`)
      return regex.test(initialData.domain)
    })

    if (wildcardDomain) {
      const wildcardBase = wildcardDomain.domain.split('*.')?.[1]
      const subdomain = initialData.domain.split(`.${wildcardBase}`)?.[0]
      return { subdomain, selectedDomain: wildcardDomain.domain }
    }

    return { subdomain: '', selectedDomain: initialData.domain }
  }

  const getInitialRedirectDomainState = () => {
    if (!initialData?.redirect_to) return { subdomain: '', selectedDomain: '' }

    const wildcardDomain = domains.find((d) => {
      const wildcardPattern = d.domain.replace('*', '(.+)')
      const regex = new RegExp(`^${wildcardPattern}$`)
      return regex.test(initialData.redirect_to ?? '')
    })

    if (wildcardDomain) {
      const wildcardBase = wildcardDomain.domain.split('*.')?.[1]
      const subdomain = initialData.redirect_to.split(`.${wildcardBase}`)?.[0]
      return { subdomain, selectedDomain: wildcardDomain.domain }
    }

    return { subdomain: '', selectedDomain: initialData.redirect_to }
  }

  const { subdomain: initialSubdomain, selectedDomain: initialSelectedDomain } =
    getInitialDomainState()
  const {
    subdomain: initialRedirectSubdomain,
    selectedDomain: initialSelectedRedirectDomain,
  } = getInitialRedirectDomainState()

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
    },
  })

  const onSubmit = (data: DomainFormValues) => {
    const payload = {
      domain: data.domain,
      environment_id: parseInt(data.environment),
      redirect_to: data.redirectTo || undefined,
      status_code: data.redirectTo ? data.statusCode : undefined,
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
  const watchedRedirectTo = useWatch({
    control: form.control,
    name: 'redirectTo',
  })
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
                <Select
                  onValueChange={(value) => {
                    setSubdomain('')
                    setSelectedDomain(value)
                    field.onChange(value)
                  }}
                  defaultValue={selectedDomain || field.value}
                >
                  <FormControl>
                    <SelectTrigger>
                      <SelectValue placeholder="Select domain" />
                    </SelectTrigger>
                  </FormControl>
                  <SelectContent>
                    {domains?.map((domain) => (
                      <SelectItem key={domain.id} value={domain.domain}>
                        {domain.domain}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>

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
                <Select
                  onValueChange={(value) => {
                    if (value === '_none_') {
                      setRedirectSubdomain('')
                      setSelectedRedirectDomain('')
                      field.onChange('')
                    } else {
                      setRedirectSubdomain('')
                      setSelectedRedirectDomain(value)
                      field.onChange(value)
                    }
                  }}
                  value={selectedRedirectDomain || field.value || '_none_'}
                >
                  <FormControl>
                    <SelectTrigger>
                      <SelectValue placeholder="No redirect" />
                    </SelectTrigger>
                  </FormControl>
                  <SelectContent>
                    <SelectItem value="_none_">No redirect</SelectItem>
                    {domains?.map((domain) => (
                      <SelectItem key={domain.id} value={domain.domain}>
                        {domain.domain}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>

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
            disabled={createDomain.isPending || updateDomain.isPending}
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
