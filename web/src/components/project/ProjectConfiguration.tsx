import { useState, useEffect, useCallback } from 'react'
import { useForm, FormProvider } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import * as z from 'zod'
import { toast } from 'sonner'
import { listServicesOptions } from '@/api/client/@tanstack/react-query.gen'
import { RepositoryResponse, ServiceTypeRoute } from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Form } from '@/components/ui/form'

import { Badge } from '@/components/ui/badge'
import { CreateServiceDialog } from '@/components/storage/CreateServiceDialog'

import { Progress } from '@/components/ui/progress'
import {
  ChevronRight,
  ChevronLeft,
  Settings,
  Loader2,
  FileText,
  Server,
  CheckCircle2,
} from 'lucide-react'
import { ProjectDetailsStep } from './wizard/ProjectDetailsStep'
import { ServicesStep } from './wizard/ServicesStep'
import { EnvironmentStep } from './wizard/EnvironmentStep'
import { ReviewStep } from './wizard/ReviewStep'

const SERVICE_TYPES = [
  {
    id: 'postgres' as ServiceTypeRoute,
    name: 'PostgreSQL',
    description: 'Reliable Relational Database',
  },
  {
    id: 'redis' as ServiceTypeRoute,
    name: 'Redis',
    description: 'In-Memory Data Store',
  },
  { id: 's3' as ServiceTypeRoute, name: 'S3', description: 'Object Storage' },
  {
    id: 'libsql' as ServiceTypeRoute,
    name: 'LibSQL',
    description: 'SQLite-compatible Database',
  },
]

// Wizard step definitions
const WIZARD_STEPS = [
  {
    id: 'project-details',
    title: 'Project Details',
    description: 'Configure basic project settings',
    icon: FileText,
    fields: [
      'name',
      'branch',
      'preset',
      'rootDirectory',
      'autoDeploy',
    ] as const,
  },
  {
    id: 'services',
    title: 'Services',
    description: 'Select and configure storage services',
    icon: Server,
    fields: ['storageServices'] as const,
  },
  {
    id: 'environment',
    title: 'Environment',
    description: 'Set up environment variables',
    icon: Settings,
    fields: ['environmentVariables'] as const,
  },
  {
    id: 'review',
    title: 'Review',
    description: 'Review and submit your configuration',
    icon: CheckCircle2,
    fields: [] as const,
  },
] as const

const formSchema = z.object({
  name: z.string().min(1, 'Project name is required'),
  preset: z.string().min(1, 'Preset is required'),
  autoDeploy: z.boolean().default(true),
  rootDirectory: z.string().default('./'),
  branch: z.string().min(1, 'Branch is required'),
  environmentVariables: z
    .array(
      z.object({
        key: z.string().min(1, 'Key is required'),
        value: z.string().min(1, 'Value is required'),
        isSecret: z.boolean().default(false),
      })
    )
    .optional(),
  storageServices: z.array(z.number()).optional(),
})

type FormValues = z.infer<typeof formSchema>

interface ProjectConfigurationProps {
  repository: RepositoryResponse
  connectionId: number
  presetData?: any
  branches?: any[]
  onSubmit: (data: FormValues, createdServices?: any[]) => Promise<void>
  isLoading?: boolean
  mode: 'onboarding' | 'import'
  className?: string
}

export function ProjectConfiguration({
  repository,
  connectionId,
  presetData,
  branches,
  onSubmit,
  isLoading = false,
  mode,
  className,
}: ProjectConfigurationProps) {
  const queryClient = useQueryClient()
  const [currentStep, setCurrentStep] = useState(0)
  const [completedSteps, setCompletedSteps] = useState<Set<number>>(new Set())
  const [isCreateServiceDialogOpen, setIsCreateServiceDialogOpen] =
    useState(false)
  const [selectedServiceType, setSelectedServiceType] =
    useState<ServiceTypeRoute | null>(null)
  const [showSecrets, setShowSecrets] = useState<{ [key: number]: boolean }>({})
  const [newlyCreatedServiceIds, setNewlyCreatedServiceIds] = useState<
    number[]
  >([])

  const form = useForm<FormValues>({
    resolver: zodResolver(formSchema),
    defaultValues: {
      name: repository.name || '',
      preset: '',
      autoDeploy: true,
      rootDirectory: './',
      branch: branches?.[0]?.name || 'main',
      environmentVariables: [],
      storageServices: [],
    },
  })

  // Auto-set preset when presetData is available
  useEffect(() => {
    if (presetData?.projects && presetData.projects.length > 0) {
      const firstPreset = presetData.projects[0]
      form.setValue('preset', firstPreset.preset || '')
      form.setValue(
        'rootDirectory',
        firstPreset.path ? `./${firstPreset.path}` : './'
      )
    } else if (presetData?.root_preset) {
      form.setValue('preset', presetData.root_preset)
      form.setValue('rootDirectory', './')
    }
  }, [presetData, form])

  // Auto-set default branch
  useEffect(() => {
    if (branches && branches.length > 0) {
      const defaultBranch =
        branches.find((b: any) => b.name === repository.default_branch) ||
        branches[0]
      if (defaultBranch) {
        form.setValue('branch', defaultBranch.name)
      }
    }
  }, [branches, repository.default_branch, form])

  // Create stable callback for service selection
  const handleServiceToggle = useCallback(
    (serviceId: number) => {
      const currentServices = form.getValues('storageServices') || []
      const isSelected = currentServices.includes(serviceId)
      const newValues = isSelected
        ? currentServices.filter((id) => id !== serviceId)
        : [...currentServices, serviceId]

      console.log('newValues', newValues)
      form.setValue('storageServices', newValues)
    },
    [form]
  )

  // Queries
  const {
    data: existingServices,
    isLoading: isServicesLoading,
    refetch: refetchServices,
  } = useQuery({
    ...listServicesOptions({}),
  })

  // Add environment variable
  const addEnvironmentVariable = () => {
    const currentVars = form.getValues('environmentVariables') || []
    form.setValue('environmentVariables', [
      ...currentVars,
      { key: '', value: '', isSecret: false },
    ])
  }

  // Remove environment variable
  const removeEnvironmentVariable = (index: number) => {
    const currentVars = form.getValues('environmentVariables') || []
    form.setValue(
      'environmentVariables',
      currentVars.filter((_, i) => i !== index)
    )
  }

  // Step validation function
  const validateCurrentStep = async () => {
    const currentStepData = WIZARD_STEPS[currentStep]
    if (currentStepData.fields.length === 0) return true // Review step has no validation

    const fieldsToValidate = currentStepData.fields as (keyof FormValues)[]
    return await form.trigger(fieldsToValidate)
  }

  // Navigation functions
  const nextStep = async () => {
    const isValid = await validateCurrentStep()
    if (isValid && currentStep < WIZARD_STEPS.length - 1) {
      setCompletedSteps((prev) => new Set([...prev, currentStep]))
      setCurrentStep((prev) => prev + 1)
    }
  }

  const previousStep = () => {
    if (currentStep > 0) {
      setCurrentStep((prev) => prev - 1)
    }
  }

  const goToStep = async (stepIndex: number) => {
    if (stepIndex < currentStep || completedSteps.has(stepIndex)) {
      setCurrentStep(stepIndex)
    } else if (stepIndex === currentStep + 1) {
      await nextStep()
    }
  }

  // Handle form submission
  const handleSubmit = async (data: FormValues) => {
    try {
      // Include newly created services in the storage services
      const allServiceIds = [
        ...(data.storageServices || []),
        ...newlyCreatedServiceIds,
      ]

      // Call parent onSubmit with data including all selected services
      await onSubmit(
        {
          ...data,
          storageServices: allServiceIds,
        },
        []
      )
    } catch (error) {
      console.error('Project configuration error:', error)
    }
  }

  const watchedEnvVars = form.watch('environmentVariables') || []
  const currentStepData = WIZARD_STEPS[currentStep]
  const progress = ((currentStep + 1) / WIZARD_STEPS.length) * 100

  // Handler functions for step components
  const handleCreateService = useCallback((serviceType: ServiceTypeRoute) => {
    setSelectedServiceType(serviceType)
    setIsCreateServiceDialogOpen(true)
  }, [])

  const handleToggleSecret = useCallback((index: number) => {
    setShowSecrets((prev) => ({ ...prev, [index]: !prev[index] }))
  }, [])

  const renderStepContent = () => {
    switch (currentStep) {
      case 0:
        return (
          <ProjectDetailsStep
            repository={repository}
            branches={branches}
            presetData={presetData}
          />
        )
      case 1:
        return (
          <ServicesStep
            existingServices={existingServices}
            newlyCreatedServiceIds={newlyCreatedServiceIds}
            onServiceToggle={handleServiceToggle}
            onCreateService={handleCreateService}
          />
        )
      case 2:
        return (
          <EnvironmentStep
            watchedEnvVars={watchedEnvVars}
            showSecrets={showSecrets}
            onAddVariable={addEnvironmentVariable}
            onRemoveVariable={removeEnvironmentVariable}
            onToggleSecret={handleToggleSecret}
          />
        )
      case 3:
        return (
          <ReviewStep
            existingServices={existingServices}
            newlyCreatedServiceIds={newlyCreatedServiceIds}
          />
        )
      default:
        return (
          <ProjectDetailsStep
            repository={repository}
            branches={branches}
            presetData={presetData}
          />
        )
    }
  }

  return (
    <div className={className}>
      <FormProvider {...form}>
        <Form {...form}>
          <form
            onSubmit={form.handleSubmit(handleSubmit)}
            className="space-y-6"
          >
            {/* Wizard Header */}
            <Card>
              <CardHeader>
                <div className="flex items-center justify-between mb-4">
                  <div>
                    <CardTitle className="flex items-center gap-2">
                      <currentStepData.icon className="h-5 w-5" />
                      {currentStepData.title}
                    </CardTitle>
                    <CardDescription>
                      {currentStepData.description}
                    </CardDescription>
                  </div>
                  <Badge variant="outline">
                    Step {currentStep + 1} of {WIZARD_STEPS.length}
                  </Badge>
                </div>

                {/* Progress Bar */}
                <Progress value={progress} className="h-2" />

                {/* Step Navigation */}
                <div className="flex items-center justify-between mt-4">
                  <div className="flex items-center gap-2">
                    {WIZARD_STEPS.map((step, index) => {
                      const Icon = step.icon
                      const isCompleted = completedSteps.has(index)
                      const isCurrent = index === currentStep
                      const isAccessible =
                        index <= currentStep || completedSteps.has(index)

                      return (
                        <button
                          key={step.id}
                          type="button"
                          onClick={() => isAccessible && goToStep(index)}
                          disabled={!isAccessible}
                          className={`flex items-center gap-2 px-3 py-2 rounded-md text-sm transition-colors ${
                            isCurrent
                              ? 'bg-primary text-primary-foreground'
                              : isCompleted
                                ? 'bg-muted text-muted-foreground hover:bg-muted/80'
                                : isAccessible
                                  ? 'hover:bg-muted text-muted-foreground'
                                  : 'text-muted-foreground/50 cursor-not-allowed'
                          }`}
                        >
                          <Icon className="h-4 w-4" />
                          <span className="hidden sm:inline">{step.title}</span>
                          {isCompleted && <CheckCircle2 className="h-4 w-4" />}
                        </button>
                      )
                    })}
                  </div>
                </div>
              </CardHeader>
            </Card>

            {/* Step Content */}
            <Card>
              <CardContent className="p-6">{renderStepContent()}</CardContent>
            </Card>

            {/* Navigation Buttons */}
            <Card>
              <CardFooter className="flex items-center justify-between">
                <Button
                  type="button"
                  variant="outline"
                  onClick={previousStep}
                  disabled={currentStep === 0}
                >
                  <ChevronLeft className="mr-2 h-4 w-4" />
                  Previous
                </Button>

                <div className="flex items-center gap-2">
                  {currentStep < WIZARD_STEPS.length - 1 ? (
                    <Button type="button" onClick={nextStep}>
                      Next
                      <ChevronRight className="ml-2 h-4 w-4" />
                    </Button>
                  ) : (
                    <Button type="submit" disabled={isLoading}>
                      {isLoading ? (
                        <>
                          <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                          {mode === 'onboarding'
                            ? 'Creating Project...'
                            : 'Importing Project...'}
                        </>
                      ) : mode === 'onboarding' ? (
                        'Create Project'
                      ) : (
                        'Import Project'
                      )}
                    </Button>
                  )}
                </div>
              </CardFooter>
            </Card>
          </form>
        </Form>
      </FormProvider>

      {/* Create Service Dialog */}
      {selectedServiceType && (
        <CreateServiceDialog
          open={isCreateServiceDialogOpen}
          onOpenChange={(open) => {
            setIsCreateServiceDialogOpen(open)
            if (!open) {
              setSelectedServiceType(null)
            }
          }}
          serviceType={selectedServiceType}
          onSuccess={(service: any) => {
            setIsCreateServiceDialogOpen(false)
            setSelectedServiceType(null)
            setNewlyCreatedServiceIds((prev) => [...prev, service.id])
            // Delay refetch to avoid immediate re-render loops
            setTimeout(() => {
              refetchServices()
            }, 100)
            toast.success(`Service "${service.name}" created successfully!`)
          }}
        />
      )}
    </div>
  )
}
