'use client'

import { createProjectFromTemplateMutation } from '@/api/client/@tanstack/react-query.gen'
import {
  ExternalServiceInfo,
  RepoSourceResponse,
  ServiceTypeRoute,
  Template,
} from '@/api/client/types.gen'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useEffect, useState } from 'react'
import { useForm } from 'react-hook-form'
import { useNavigate } from 'react-router-dom'
import { toast } from 'sonner'
import * as z from 'zod'
import { Button } from '../ui/button'
import { Trash, Plus } from 'lucide-react'

import { TemplateGitHub } from '@/api/client/types.gen'
import FrameworkIcon from '@/components/project/FrameworkIcon'
import {
  Card,
  CardContent,
  CardFooter,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
import {
  Form,
  FormControl,
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
import { Separator } from '@/components/ui/separator'
import { GitBranchIcon, GithubIcon } from 'lucide-react'
import { CreateServiceDialog } from '@/components/storage/CreateServiceDialog'

// Move all the form-related types and schemas here
const formSchema = z
  .object({
    name: z.string().min(1, 'Project name is required'),
    autoDeploy: z.boolean().default(true),
    useTemplateRepo: z.boolean().default(false),
    account: z.string().optional(),
    destinationRepo: z.string().optional(),
    environmentVariables: z.array(
      z
        .object({
          key: z.string(),
          value: z.string(),
          isRequired: z.boolean().default(false),
        })
        .refine(
          (env) => {
            // Allow completely empty env vars (both key and value empty)
            if (env.key === '' && env.value === '') return true
            // If one is filled, both must be filled
            if (env.key !== '' || env.value !== '') {
              return env.key !== '' && env.value !== ''
            }
            return true
          },
          {
            message: 'Both key and value are required if either is filled',
          }
        )
    ),
    storageServices: z.array(z.number()).optional(),
  })
  .refine(
    (data) => {
      if (!data.useTemplateRepo) {
        return data.account && data.destinationRepo
      }
      return true
    },
    {
      message:
        'Team and destination repository are required when not using template repository',
      path: ['account'],
    }
  )

type FormValues = z.infer<typeof formSchema>

const slugify = (str: string) => {
  return str
    .toLowerCase()
    .trim()
    .replace(/[^\w\s-]/g, '')
    .replace(/[\s_-]+/g, '-')
    .replace(/^-+|-+$/g, '')
}

const findServiceByType = (
  services: ExternalServiceInfo[],
  serviceType: string
) => {
  return services.find((service) => service.service_type === serviceType)
}

interface ImportTemplateFormProps {
  template: Template
  sources: RepoSourceResponse[]
  storageServices: ExternalServiceInfo[]
  reloadServices: () => void
}

export function ImportTemplateForm({
  template,
  sources,
  storageServices,
  reloadServices,
}: ImportTemplateFormProps) {
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const [missingServices, setMissingServices] = useState<string[]>([])
  const [openServiceType, setOpenServiceType] =
    useState<ServiceTypeRoute | null>(null)

  const createProjectMutation = useMutation({
    ...createProjectFromTemplateMutation(),
    meta: {
      errorTitle: 'Failed to create project from template',
    },
    onSuccess: async (data) => {
      // Invalidate projects queries to refresh the command palette
      await queryClient.invalidateQueries({ queryKey: ['getProjects'] })
      await queryClient.invalidateQueries({ queryKey: ['listProjects'] })
      toast.success('Project created successfully')
      navigate(`/projects/${data.slug}?new=true`)
    },
  })

  const form = useForm<FormValues>({
    resolver: zodResolver(formSchema),
    defaultValues: {
      name: template.name ? slugify(template.name) : '',
      autoDeploy: true,
      useTemplateRepo: false,
      account: '',
      destinationRepo: '',
      environmentVariables: template.env
        ? template.env.map((env) => ({
            key: env.name,
            value: env.default || '',
            isRequired: true,
          }))
        : [{ key: '', value: '', isRequired: false }],
      storageServices: [],
    },
  })

  useEffect(() => {
    if (template.services) {
      const missing = template.services.filter(
        (requiredType) => !findServiceByType(storageServices, requiredType)
      )
      setMissingServices(missing)

      const availableServices = template.services
        .map((type) => findServiceByType(storageServices, type))
        .filter(
          (service): service is NonNullable<typeof service> =>
            service !== undefined
        )
        .map((service) => service.id)

      // Auto-select required services
      if (availableServices.length > 0) {
        form.setValue('storageServices', availableServices)
      }
    }
  }, [template.services, storageServices, form])

  const onSubmit = async (data: FormValues) => {
    if (template.services && template.services.length > 0) {
      const selectedServices =
        storageServices?.filter((service) =>
          data.storageServices?.includes(service.id)
        ) || []
      const missingTypes = template.services.filter(
        (requiredType) =>
          !selectedServices.some(
            (service) => service.service_type === requiredType
          )
      )

      if (missingTypes.length > 0) {
        toast.error(`Missing required services: ${missingTypes.join(', ')}`)
        return
      }
    }

    let githubName: string
    let githubOwner: string

    if (data.useTemplateRepo && template.github) {
      const githubInfo = template.github as TemplateGitHub
      githubName = githubInfo.path.split('/').pop() || githubInfo.path
      githubOwner = githubInfo.owner
    } else {
      githubName = data.destinationRepo || ''
      githubOwner = data.account || ''
    }

    // Filter out empty environment variables (both key and value empty)
    const validEnvVars = data.environmentVariables.filter(
      (env) => env.key.trim() !== '' || env.value.trim() !== ''
    )

    await createProjectMutation.mutateAsync({
      body: {
        ...data,
        github_name: githubName,
        github_owner: githubOwner,
        automatic_deploy: data.autoDeploy,
        project_name: data.name,
        template_name: template.name,
        environment_variables: validEnvVars.map((env) => [env.key, env.value]),
        storage_service_ids: data.storageServices || [],
      },
    })
  }

  return (
    <div className="min-h-screen bg-background text-foreground p-8">
      <h1 className="text-4xl font-bold mb-2">Create from template</h1>
      <p className="text-muted-foreground mb-8">
        Configure your new project based on {template.name}
      </p>

      <div className="flex gap-8">
        {/* Left sidebar */}
        <div className="w-1/3">
          <Card className="bg-card text-card-foreground mb-4">
            <CardContent className="p-4">
              <div className="mb-4 rounded-lg overflow-hidden">
                <img
                  src={`/api/templates/${template.name}/preview`}
                  alt={`${template.name} preview`}
                  className="w-full h-auto object-cover"
                />
              </div>
              <div className="flex items-center gap-2">
                <FrameworkIcon preset={template.preset as any} />
                <span>{template.name}</span>
              </div>
              <p className="text-sm text-muted-foreground mt-2">
                {template.description}
              </p>
            </CardContent>
          </Card>

          <div className="space-y-2 mb-8">
            <div className="flex items-center">
              <div className="w-2 h-2 bg-primary rounded-full mr-2"></div>
              <span className="font-medium">Configure Project</span>
            </div>
            <div className="flex items-center text-muted-foreground">
              <div className="w-2 h-2 bg-muted rounded-full mr-2"></div>
              <span>Deploy</span>
            </div>
          </div>

          <Separator className="my-4" />

          <div className="text-sm text-muted-foreground">
            <h3 className="font-medium mb-2">TEMPLATE DETAILS</h3>
            <div className="flex items-center mb-1">
              <GithubIcon className="mr-2 h-4 w-4" />
              <span>
                {(template.github! as TemplateGitHub).owner}/
                {(template.github! as TemplateGitHub).path}
              </span>
            </div>
            <div className="flex items-center">
              <GitBranchIcon className="mr-2 h-4 w-4" />
              <span>main</span>
            </div>
          </div>
        </div>

        {/* Main form */}
        <Card className="w-2/3">
          <CardHeader>
            <CardTitle>Configure Project</CardTitle>
          </CardHeader>
          <Form {...form}>
            <form onSubmit={form.handleSubmit(onSubmit)}>
              <CardContent className="space-y-6">
                <FormField
                  control={form.control}
                  name="name"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Project Name</FormLabel>
                      <FormControl>
                        <Input {...field} placeholder="my-awesome-project" />
                      </FormControl>
                      <FormMessage />
                    </FormItem>
                  )}
                />

                <FormField
                  control={form.control}
                  name="useTemplateRepo"
                  render={({ field }) => (
                    <FormItem className="flex flex-row items-start space-x-3 space-y-0 rounded-md border p-4">
                      <FormControl>
                        <Checkbox
                          checked={field.value}
                          onCheckedChange={field.onChange}
                        />
                      </FormControl>
                      <div className="space-y-1 leading-none">
                        <FormLabel>Use Template Repository</FormLabel>
                        <p className="text-sm text-muted-foreground">
                          Use the public template repository directly:{' '}
                          {(template.github as TemplateGitHub)?.owner}/
                          {(template.github as TemplateGitHub)?.path}
                        </p>
                      </div>
                    </FormItem>
                  )}
                />
                {!form.watch('useTemplateRepo') && (
                  <div className="flex gap-4">
                    <FormField
                      control={form.control}
                      name="account"
                      render={({ field }) => {
                        return (
                          <FormItem className="flex-1">
                            <FormLabel>Select Team</FormLabel>
                            <Select
                              onValueChange={(value) => {
                                if (value !== 'add_account') {
                                  field.onChange(value)
                                } else if (value === 'add_account') {
                                  window.location.href = '/api/github/login'
                                }
                              }}
                              value={field.value}
                              defaultValue={field.value}
                            >
                              <FormControl>
                                <SelectTrigger>
                                  <SelectValue placeholder="Select an account" />
                                </SelectTrigger>
                              </FormControl>
                              <SelectContent>
                                {sources?.map((source) => (
                                  <SelectItem
                                    key={source.name}
                                    value={source.name}
                                  >
                                    {source.name}
                                  </SelectItem>
                                ))}
                                <SelectItem
                                  value="add_account"
                                  className="text-primary hover:text-primary/90 cursor-pointer"
                                >
                                  Add GitHub Account
                                </SelectItem>
                              </SelectContent>
                            </Select>
                            <FormMessage />
                          </FormItem>
                        )
                      }}
                    />

                    <FormField
                      control={form.control}
                      name="destinationRepo"
                      render={({ field }) => (
                        <FormItem className="flex-1">
                          <FormLabel>Destination Repository</FormLabel>
                          <FormControl>
                            <Input {...field} placeholder="Repository name" />
                          </FormControl>
                          <FormMessage />
                        </FormItem>
                      )}
                    />
                  </div>
                )}

                {form.watch('useTemplateRepo') && (
                  <div className="rounded-md border p-4 bg-muted/50">
                    <p className="text-sm text-muted-foreground mb-2">
                      Repository Source:
                    </p>
                    <div className="flex items-center gap-2">
                      <GithubIcon className="h-4 w-4" />
                      <span className="font-medium">
                        {(template.github as TemplateGitHub)?.owner}/
                        {(template.github as TemplateGitHub)?.path}
                      </span>
                    </div>
                  </div>
                )}

                <FormField
                  control={form.control}
                  name="autoDeploy"
                  render={({ field }) => (
                    <FormItem className="flex flex-row items-start space-x-3 space-y-0 rounded-md border p-4">
                      <FormControl>
                        <Checkbox
                          checked={field.value}
                          onCheckedChange={field.onChange}
                        />
                      </FormControl>
                      <div className="space-y-1 leading-none">
                        <FormLabel>Automatic Deployments</FormLabel>
                        <p className="text-sm text-muted-foreground">
                          Automatically deploy when changes are pushed to the
                          repository
                        </p>
                      </div>
                    </FormItem>
                  )}
                />

                <FormField
                  control={form.control}
                  name="environmentVariables"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Environment Variables</FormLabel>
                      <div className="space-y-4">
                        {field.value?.map((envVar, index) => (
                          <div key={index} className="flex flex-col space-y-2">
                            <div className="flex items-center space-x-2">
                              <Input
                                placeholder="Key"
                                value={envVar.key}
                                disabled={envVar.isRequired}
                                onChange={(e) => {
                                  const newEnvVars = [...(field.value || [])]
                                  newEnvVars[index].key = e.target.value
                                  field.onChange(newEnvVars)
                                }}
                              />
                              <Input
                                placeholder={
                                  template.env?.[index]?.example || 'Value'
                                }
                                value={envVar.value}
                                onChange={(e) => {
                                  const newEnvVars = [...(field.value || [])]
                                  newEnvVars[index].value = e.target.value
                                  field.onChange(newEnvVars)
                                }}
                              />
                              {!envVar.isRequired && (
                                <Button
                                  variant="ghost"
                                  size="icon"
                                  type="button"
                                  onClick={() => {
                                    const newEnvVars = field.value?.filter(
                                      (_, i) => i !== index
                                    )
                                    field.onChange(newEnvVars)
                                  }}
                                >
                                  <Trash className="h-4 w-4" />
                                </Button>
                              )}
                            </div>
                            {envVar.isRequired && !envVar.value && (
                              <p className="text-sm text-destructive">
                                This environment variable is required
                              </p>
                            )}
                          </div>
                        ))}
                        <Button
                          variant="outline"
                          type="button"
                          onClick={() => {
                            field.onChange([
                              ...(field.value || []),
                              { key: '', value: '', isRequired: false },
                            ])
                          }}
                        >
                          <Plus className="h-4 w-4 mr-2" />
                          Add Variable
                        </Button>
                      </div>
                    </FormItem>
                  )}
                />
                {template.services && template.services.length > 0 && (
                  <FormField
                    control={form.control}
                    name="storageServices"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Required Services</FormLabel>
                        {/* Show missing service types that aren't in storageServices */}
                        {missingServices.filter(
                          (serviceType) =>
                            !storageServices.some(
                              (s) => s.service_type === serviceType
                            )
                        ).length > 0 && (
                          <div className="mb-4">
                            <p className="text-sm font-medium text-destructive mb-2">
                              Missing Required Services
                            </p>
                            <div className="space-y-2">
                              {missingServices
                                .filter(
                                  (serviceType) =>
                                    !storageServices.some(
                                      (s) => s.service_type === serviceType
                                    )
                                )
                                .map((serviceType) => (
                                  <div
                                    key={serviceType}
                                    className="flex items-start justify-between space-y-0 rounded-md border border-destructive bg-destructive/5 p-4"
                                  >
                                    <div className="space-y-1">
                                      <div className="font-medium">
                                        {serviceType}
                                        <span className="ml-2 text-sm text-destructive">
                                          (Required)
                                        </span>
                                      </div>
                                      <p className="text-sm text-destructive">
                                        This required service type needs to be
                                        created
                                      </p>
                                    </div>
                                    <Button
                                      type="button"
                                      variant="destructive"
                                      size="sm"
                                      onClick={() =>
                                        setOpenServiceType(
                                          serviceType as ServiceTypeRoute
                                        )
                                      }
                                    >
                                      Create Service
                                    </Button>
                                  </div>
                                ))}
                            </div>
                          </div>
                        )}

                        {/* Existing services section */}
                        <div className="space-y-2">
                          {storageServices.map((service) => {
                            const isRequired = template.services?.includes(
                              service.service_type
                            )
                            const isServiceTypeMissing =
                              missingServices.includes(service.service_type)
                            return (
                              <div
                                key={service.id}
                                className={`flex items-start justify-between space-y-0 rounded-md border p-4 ${
                                  isRequired && isServiceTypeMissing
                                    ? 'border-destructive bg-destructive/5'
                                    : ''
                                }`}
                              >
                                <div className="flex items-start space-x-3">
                                  <FormControl>
                                    <Checkbox
                                      checked={field.value?.includes(
                                        service.id
                                      )}
                                      onCheckedChange={(checked) => {
                                        const updatedValue = checked
                                          ? [...(field.value || []), service.id]
                                          : field.value?.filter(
                                              (id) => id !== service.id
                                            ) || []
                                        field.onChange(updatedValue)
                                      }}
                                    />
                                  </FormControl>
                                  <div className="space-y-1 leading-none">
                                    <div className="font-medium">
                                      {service.name}
                                      {isRequired && (
                                        <span
                                          className={`ml-2 text-sm ${isServiceTypeMissing ? 'text-destructive' : 'text-muted-foreground'}`}
                                        >
                                          (Required)
                                        </span>
                                      )}
                                    </div>
                                    <p className="text-sm text-muted-foreground">
                                      {service.service_type}
                                    </p>
                                    {isRequired && isServiceTypeMissing && (
                                      <p className="text-sm text-destructive mt-1">
                                        This required service needs to be
                                        created
                                      </p>
                                    )}
                                  </div>
                                </div>
                                {isRequired && isServiceTypeMissing && (
                                  <Button
                                    type="button"
                                    variant="destructive"
                                    size="sm"
                                    onClick={() =>
                                      setOpenServiceType(service.service_type)
                                    }
                                  >
                                    Create Service
                                  </Button>
                                )}
                              </div>
                            )
                          })}
                        </div>
                        <FormMessage />
                        <CreateServiceDialog
                          onSuccess={(service) => {
                            form.setValue('storageServices', [
                              ...(form.getValues('storageServices') || []),
                              service.id,
                            ])
                            reloadServices()
                            setOpenServiceType(null)
                          }}
                          open={!!openServiceType}
                          onOpenChange={() => setOpenServiceType(null)}
                          serviceType={openServiceType as ServiceTypeRoute}
                        />
                      </FormItem>
                    )}
                  />
                )}
              </CardContent>
              <CardFooter>
                <Button
                  type="submit"
                  className="w-full"
                  disabled={createProjectMutation.isPending}
                >
                  {createProjectMutation.isPending
                    ? 'Creating...'
                    : 'Create Project'}
                </Button>
              </CardFooter>
            </form>
          </Form>
        </Card>
      </div>
    </div>
  )
}
