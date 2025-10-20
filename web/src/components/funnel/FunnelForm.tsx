import {
  getUniqueEventsOptions,
  previewFunnelMetricsMutation,
} from '@/api/client/@tanstack/react-query.gen'
import {
  CreateFunnelStep,
  ProjectResponse,
  SmartFilter,
} from '@/api/client/types.gen'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  Command,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from '@/components/ui/command'
import { Input } from '@/components/ui/input'
import { JsonEditor } from '@/components/ui/json-editor'
import { Label } from '@/components/ui/label'
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Separator } from '@/components/ui/separator'
import { Skeleton } from '@/components/ui/skeleton'
import { Textarea } from '@/components/ui/textarea'
import { cn } from '@/lib/utils'
import { useMutation, useQuery } from '@tanstack/react-query'
import {
  Check,
  ChevronDown,
  ChevronsUpDown,
  ChevronUp,
  Clock,
  Filter,
  Percent,
  Plus,
  Trash2,
  TrendingDown,
  Users,
} from 'lucide-react'
import * as React from 'react'

type FilterType = SmartFilter['type']

interface StepWithFilters extends CreateFunnelStep {
  showFilters: boolean
}

export interface FunnelFormData {
  name: string
  description: string
  steps: StepWithFilters[]
}

interface FunnelFormProps {
  project: ProjectResponse
  initialData?: FunnelFormData
  isSubmitting: boolean
  feedback: { type: 'success' | 'error'; message: string } | null
  onSubmit: (data: FunnelFormData) => void
  onCancel: () => void
  submitLabel?: string
  title: string
  description: string
}

const FILTER_TYPES: {
  value: FilterType
  label: string
  placeholder: string
  isJson?: boolean
}[] = [
  { value: 'page_path', label: 'Page Path', placeholder: '/checkout' },
  { value: 'hostname', label: 'Hostname', placeholder: 'example.com' },
  { value: 'utm_source', label: 'UTM Source', placeholder: 'google' },
  { value: 'utm_campaign', label: 'UTM Campaign', placeholder: 'summer-sale' },
  { value: 'utm_medium', label: 'UTM Medium', placeholder: 'cpc' },
  { value: 'referrer_hostname', label: 'Referrer', placeholder: 'google.com' },
  { value: 'channel', label: 'Channel', placeholder: 'organic' },
  { value: 'device_type', label: 'Device Type', placeholder: 'mobile' },
  { value: 'browser', label: 'Browser', placeholder: 'chrome' },
  {
    value: 'operating_system',
    label: 'Operating System',
    placeholder: 'windows',
  },
  { value: 'language', label: 'Language', placeholder: 'en' },
  {
    value: 'custom_data',
    label: 'Custom Data',
    placeholder: '{"key": "value"}',
    isJson: true,
  },
]

// Validate JSON for custom_data filters
const validateCustomDataJson = (
  jsonString: string
): { valid: boolean; error?: string } => {
  try {
    const parsed = JSON.parse(jsonString)

    // Must be a plain object
    if (
      typeof parsed !== 'object' ||
      parsed === null ||
      Array.isArray(parsed)
    ) {
      return { valid: false, error: 'Must be a plain JSON object' }
    }

    // Check all values are strings
    for (const [key, value] of Object.entries(parsed)) {
      if (typeof value !== 'string') {
        return {
          valid: false,
          error: `Value for key "${key}" must be a string`,
        }
      }
    }

    return { valid: true }
  } catch {
    return { valid: false, error: 'Invalid JSON format' }
  }
}

export function FunnelForm({
  project,
  initialData,
  isSubmitting,
  feedback,
  onSubmit,
  onCancel,
  submitLabel = 'Create Funnel',
  title,
  description,
}: FunnelFormProps) {
  const [formData, setFormData] = React.useState<FunnelFormData>(
    initialData || {
      name: '',
      description: '',
      steps: [
        {
          event_name: 'page_view',
          event_filter: [],
          showFilters: false,
        },
      ],
    }
  )

  // Track validation errors for custom_data filters
  const [filterValidationErrors, setFilterValidationErrors] = React.useState<
    Record<string, Record<number, string>>
  >({})

  // Track popover open state for each step
  const [openPopovers, setOpenPopovers] = React.useState<
    Record<number, boolean>
  >({})

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault()

    // Validate all custom_data filters before submitting
    let hasValidationErrors = false
    const newErrors: Record<string, Record<number, string>> = {}

    formData.steps.forEach((step, stepIndex) => {
      step.event_filter?.forEach((filter, filterIndex) => {
        if (filter.type === 'custom_data') {
          const filterValue =
            typeof filter.value === 'object'
              ? JSON.stringify(filter.value)
              : filter.value
          const validation = validateCustomDataJson(filterValue)
          if (!validation.valid) {
            hasValidationErrors = true
            if (!newErrors[stepIndex]) {
              newErrors[stepIndex] = {}
            }
            newErrors[stepIndex][filterIndex] =
              validation.error || 'Invalid JSON'
          }
        }
      })
    })

    setFilterValidationErrors(newErrors)

    if (hasValidationErrors) {
      return // Don't submit if there are validation errors
    }

    onSubmit(formData)
  }

  const addStep = () => {
    setFormData((prev) => ({
      ...prev,
      steps: [
        ...prev.steps,
        { event_name: '', event_filter: [], showFilters: false },
      ],
    }))
  }

  const updateStep = (
    index: number,
    field: keyof CreateFunnelStep,
    value: string
  ) => {
    setFormData((prev) => ({
      ...prev,
      steps: prev.steps.map((step, i) =>
        i === index ? { ...step, [field]: value } : step
      ),
    }))
  }

  const toggleFilters = (index: number) => {
    setFormData((prev) => ({
      ...prev,
      steps: prev.steps.map((step, i) =>
        i === index ? { ...step, showFilters: !step.showFilters } : step
      ),
    }))
  }

  const addFilter = (stepIndex: number) => {
    const newFilter: SmartFilter = { type: 'page_path', value: '' }
    setFormData((prev) => ({
      ...prev,
      steps: prev.steps.map((step, i) =>
        i === stepIndex
          ? { ...step, event_filter: [...(step.event_filter || []), newFilter] }
          : step
      ),
    }))
  }

  const updateFilter = (
    stepIndex: number,
    filterIndex: number,
    field: 'type' | 'value',
    value: string
  ) => {
    // Validate custom_data JSON on change
    if (field === 'value') {
      const currentFilter =
        formData.steps[stepIndex]?.event_filter?.[filterIndex]
      if (currentFilter?.type === 'custom_data') {
        const validation = validateCustomDataJson(value)
        setFilterValidationErrors((prev) => {
          const newErrors = { ...prev }
          if (!validation.valid) {
            if (!newErrors[stepIndex]) {
              newErrors[stepIndex] = {}
            }
            newErrors[stepIndex][filterIndex] =
              validation.error || 'Invalid JSON'
          } else {
            // Clear error if valid
            if (newErrors[stepIndex]) {
              delete newErrors[stepIndex][filterIndex]
              if (Object.keys(newErrors[stepIndex]).length === 0) {
                delete newErrors[stepIndex]
              }
            }
          }
          return newErrors
        })
      }
    }

    // Clear validation error when changing type (separate state update)
    if (field === 'type') {
      setFilterValidationErrors((prev) => {
        const newErrors = { ...prev }
        if (newErrors[stepIndex]) {
          delete newErrors[stepIndex][filterIndex]
          if (Object.keys(newErrors[stepIndex]).length === 0) {
            delete newErrors[stepIndex]
          }
        }
        return newErrors
      })
    }

    // Update form data
    setFormData((prev) => ({
      ...prev,
      steps: prev.steps.map((step, i) => {
        if (i !== stepIndex) return step
        const filters = [...(step.event_filter || [])]
        if (field === 'type') {
          const newType = value as FilterType
          // Reset value based on filter type
          if (newType === 'custom_data') {
            filters[filterIndex] = {
              type: newType,
              value: { path: '', value: '' },
            }
          } else {
            filters[filterIndex] = { type: newType, value: '' }
          }
        } else {
          // When updating value, preserve the type and create proper discriminated union
          const currentFilter = filters[filterIndex]
          if (currentFilter.type === 'custom_data') {
            // For custom_data, value comes as JSON string from the editor
            // Parse it to get the object, or use current value if parse fails
            let parsedValue: { path: string; value: string }
            try {
              parsedValue = JSON.parse(value)
            } catch {
              // If parsing fails, keep current value or use empty object
              parsedValue =
                typeof currentFilter.value === 'object'
                  ? currentFilter.value
                  : { path: '', value: '' }
            }
            filters[filterIndex] = {
              type: 'custom_data',
              value: parsedValue,
            }
          } else {
            filters[filterIndex] = {
              type: currentFilter.type,
              value: value,
            }
          }
        }
        return { ...step, event_filter: filters }
      }),
    }))
  }

  const removeFilter = (stepIndex: number, filterIndex: number) => {
    // Clear validation error when removing filter
    setFilterValidationErrors((prev) => {
      const newErrors = { ...prev }
      if (newErrors[stepIndex]) {
        delete newErrors[stepIndex][filterIndex]
        if (Object.keys(newErrors[stepIndex]).length === 0) {
          delete newErrors[stepIndex]
        }
      }
      return newErrors
    })

    setFormData((prev) => ({
      ...prev,
      steps: prev.steps.map((step, i) =>
        i === stepIndex
          ? {
              ...step,
              event_filter: step.event_filter?.filter(
                (_, j) => j !== filterIndex
              ),
            }
          : step
      ),
    }))
  }

  const removeStep = (index: number) => {
    setFormData((prev) => ({
      ...prev,
      steps: prev.steps.filter((_, i) => i !== index),
    }))
  }

  // Fetch unique events for autocomplete
  const { data: uniqueEventsData } = useQuery({
    ...getUniqueEventsOptions({
      path: {
        project_id: project.id,
      },
    }),
  })

  const uniqueEvents = uniqueEventsData?.events || []

  // Preview funnel metrics using real API
  const previewMutation = useMutation({
    ...previewFunnelMetricsMutation(),
    meta: {
      errorTitle: 'Failed to preview funnel metrics',
    },
  })

  // Debounced preview update
  React.useEffect(() => {
    // Only fetch preview if we have valid steps (name is not required)
    if (formData.steps.length === 0) {
      return
    }

    const validSteps = formData.steps
      .filter((step) => step.event_name.trim())
      .map(({ showFilters: _showFilters, ...step }) => step)

    if (validSteps.length === 0) {
      return
    }

    const timer = setTimeout(() => {
      previewMutation.mutate({
        path: {
          project_id: project.id,
        },
        body: {
          name: formData.name.trim() || 'Preview',
          description: formData.description.trim() || undefined,
          steps: validSteps,
        },
      })
    }, 500) // Debounce for 500ms

    return () => clearTimeout(timer)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [formData.name, formData.description, formData.steps, project.id])

  return (
    <div className="w-full max-w-7xl mx-auto space-y-6 p-6">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-3xl font-bold">{title}</h1>
          <p className="text-muted-foreground">{description}</p>
        </div>
      </div>

      {feedback && (
        <Alert variant={feedback.type === 'error' ? 'destructive' : 'default'}>
          <AlertDescription>{feedback.message}</AlertDescription>
        </Alert>
      )}

      <div className="grid grid-cols-1 lg:grid-cols-2 gap-6">
        {/* Form Section */}
        <div className="space-y-6">
          <Card>
            <CardHeader>
              <CardTitle>Funnel Configuration</CardTitle>
              <CardDescription>
                Set up your funnel name, description, and conversion steps
              </CardDescription>
            </CardHeader>
            <CardContent>
              <form onSubmit={handleSubmit} className="space-y-6">
                <div className="space-y-2">
                  <Label htmlFor="name">Funnel Name *</Label>
                  <Input
                    id="name"
                    value={formData.name}
                    onChange={(e) =>
                      setFormData((prev) => ({ ...prev, name: e.target.value }))
                    }
                    placeholder="e.g., User Onboarding, Checkout Flow"
                    required
                  />
                </div>

                <div className="space-y-2">
                  <Label htmlFor="description">Description</Label>
                  <Textarea
                    id="description"
                    value={formData.description}
                    onChange={(e) =>
                      setFormData((prev) => ({
                        ...prev,
                        description: e.target.value,
                      }))
                    }
                    placeholder="Brief description of what this funnel tracks"
                    rows={3}
                  />
                </div>

                <Separator />

                <div className="space-y-4">
                  <div className="flex items-center justify-between">
                    <Label className="text-base">Funnel Steps *</Label>
                    <Button
                      type="button"
                      variant="outline"
                      size="sm"
                      onClick={addStep}
                    >
                      <Plus className="h-3 w-3 mr-1" />
                      Add Step
                    </Button>
                  </div>

                  {formData.steps.map((step, index) => (
                    <Card key={index} className="border-2">
                      <CardContent className="pt-4 space-y-4">
                        <div className="flex gap-3">
                          <div className="flex-shrink-0 w-8 h-8 bg-primary text-primary-foreground rounded-full flex items-center justify-center font-semibold">
                            {index + 1}
                          </div>
                          <div className="flex-1 space-y-3">
                            <div className="flex gap-2">
                              <Popover
                                open={openPopovers[index]}
                                onOpenChange={(open) =>
                                  setOpenPopovers((prev) => ({
                                    ...prev,
                                    [index]: open,
                                  }))
                                }
                              >
                                <PopoverTrigger asChild>
                                  <Button
                                    variant="outline"
                                    role="combobox"
                                    className="flex-1 justify-between font-normal"
                                  >
                                    {step.event_name ||
                                      (index === 0
                                        ? 'Select entry event...'
                                        : 'Select event...')}
                                    <ChevronsUpDown className="ml-2 h-4 w-4 shrink-0 opacity-50" />
                                  </Button>
                                </PopoverTrigger>
                                <PopoverContent
                                  className="w-[400px] p-0"
                                  align="start"
                                >
                                  <Command>
                                    <CommandInput
                                      placeholder="Search events or type custom name..."
                                      value={step.event_name}
                                      onValueChange={(value) =>
                                        updateStep(index, 'event_name', value)
                                      }
                                    />
                                    <CommandList>
                                      <CommandEmpty>
                                        <div className="py-6 text-center text-sm">
                                          <p className="text-muted-foreground mb-2">
                                            No matching events found
                                          </p>
                                          <p className="text-xs text-muted-foreground">
                                            Press Enter to use &quot;
                                            {step.event_name}&quot; as custom
                                            event
                                          </p>
                                        </div>
                                      </CommandEmpty>
                                      {uniqueEvents.length > 0 && (
                                        <CommandGroup heading="Available Events">
                                          {uniqueEvents.map((event) => (
                                            <CommandItem
                                              key={event.name}
                                              value={event.name}
                                              onSelect={(value) => {
                                                updateStep(
                                                  index,
                                                  'event_name',
                                                  value
                                                )
                                                setOpenPopovers((prev) => ({
                                                  ...prev,
                                                  [index]: false,
                                                }))
                                              }}
                                            >
                                              <Check
                                                className={cn(
                                                  'mr-2 h-4 w-4',
                                                  step.event_name === event.name
                                                    ? 'opacity-100'
                                                    : 'opacity-0'
                                                )}
                                              />
                                              <div className="flex items-center justify-between flex-1">
                                                <span>{event.name}</span>
                                                <Badge
                                                  variant="secondary"
                                                  className="ml-2"
                                                >
                                                  {event.count.toLocaleString()}
                                                </Badge>
                                              </div>
                                            </CommandItem>
                                          ))}
                                        </CommandGroup>
                                      )}
                                    </CommandList>
                                  </Command>
                                </PopoverContent>
                              </Popover>
                              {formData.steps.length > 1 && (
                                <Button
                                  type="button"
                                  variant="ghost"
                                  size="icon"
                                  onClick={() => removeStep(index)}
                                >
                                  <Trash2 className="h-4 w-4" />
                                </Button>
                              )}
                            </div>

                            <Button
                              type="button"
                              variant="outline"
                              size="sm"
                              onClick={() => toggleFilters(index)}
                              className="w-full"
                            >
                              <Filter className="h-3 w-3 mr-2" />
                              {step.showFilters ? 'Hide' : 'Show'} Filters
                              {step.event_filter &&
                                step.event_filter.length > 0 && (
                                  <Badge variant="secondary" className="ml-2">
                                    {step.event_filter.length}
                                  </Badge>
                                )}
                              {step.showFilters ? (
                                <ChevronUp className="h-3 w-3 ml-auto" />
                              ) : (
                                <ChevronDown className="h-3 w-3 ml-auto" />
                              )}
                            </Button>

                            {step.showFilters && (
                              <div className="space-y-3 pt-2 border-t">
                                <div className="flex items-center justify-between">
                                  <Label className="text-sm">
                                    Event Filters
                                  </Label>
                                  <Button
                                    type="button"
                                    variant="ghost"
                                    size="sm"
                                    onClick={() => addFilter(index)}
                                  >
                                    <Plus className="h-3 w-3 mr-1" />
                                    Add Filter
                                  </Button>
                                </div>

                                {step.event_filter &&
                                step.event_filter.length > 0 ? (
                                  <div className="space-y-3">
                                    {step.event_filter.map(
                                      (filter, filterIndex) => {
                                        const filterType = FILTER_TYPES.find(
                                          (ft) => ft.value === filter.type
                                        )
                                        const hasError =
                                          filterValidationErrors[index]?.[
                                            filterIndex
                                          ]
                                        const isJsonType = filterType?.isJson

                                        return (
                                          <div
                                            key={filterIndex}
                                            className="space-y-2"
                                          >
                                            <div className="flex gap-2 items-start">
                                              <Select
                                                value={filter.type}
                                                onValueChange={(value) =>
                                                  updateFilter(
                                                    index,
                                                    filterIndex,
                                                    'type',
                                                    value
                                                  )
                                                }
                                              >
                                                <SelectTrigger className="w-[180px]">
                                                  <SelectValue />
                                                </SelectTrigger>
                                                <SelectContent>
                                                  {FILTER_TYPES.map((ft) => (
                                                    <SelectItem
                                                      key={ft.value}
                                                      value={ft.value}
                                                    >
                                                      {ft.label}
                                                    </SelectItem>
                                                  ))}
                                                </SelectContent>
                                              </Select>

                                              {isJsonType ? (
                                                <div className="flex-1 space-y-1">
                                                  <div
                                                    className={`rounded-md border overflow-hidden ${
                                                      hasError
                                                        ? 'border-destructive'
                                                        : 'border-input'
                                                    }`}
                                                  >
                                                    <JsonEditor
                                                      value={
                                                        filter.type ===
                                                        'custom_data'
                                                          ? typeof filter.value ===
                                                            'object'
                                                            ? JSON.stringify(
                                                                filter.value,
                                                                null,
                                                                2
                                                              )
                                                            : filter.value || ''
                                                          : filter.value || ''
                                                      }
                                                      onChange={(value) =>
                                                        updateFilter(
                                                          index,
                                                          filterIndex,
                                                          'value',
                                                          value
                                                        )
                                                      }
                                                      onValidationChange={(
                                                        isValid,
                                                        error
                                                      ) => {
                                                        if (!isValid && error) {
                                                          setFilterValidationErrors(
                                                            (prev) => {
                                                              const newErrors =
                                                                { ...prev }
                                                              if (
                                                                !newErrors[
                                                                  index
                                                                ]
                                                              ) {
                                                                newErrors[
                                                                  index
                                                                ] = {}
                                                              }
                                                              newErrors[index][
                                                                filterIndex
                                                              ] = error
                                                              return newErrors
                                                            }
                                                          )
                                                        } else {
                                                          setFilterValidationErrors(
                                                            (prev) => {
                                                              const newErrors =
                                                                { ...prev }
                                                              if (
                                                                newErrors[index]
                                                              ) {
                                                                delete newErrors[
                                                                  index
                                                                ][filterIndex]
                                                                if (
                                                                  Object.keys(
                                                                    newErrors[
                                                                      index
                                                                    ]
                                                                  ).length === 0
                                                                ) {
                                                                  delete newErrors[
                                                                    index
                                                                  ]
                                                                }
                                                              }
                                                              return newErrors
                                                            }
                                                          )
                                                        }
                                                      }}
                                                      height="120px"
                                                    />
                                                  </div>
                                                  {hasError && (
                                                    <p className="text-xs text-destructive">
                                                      {hasError}
                                                    </p>
                                                  )}
                                                </div>
                                              ) : (
                                                <Input
                                                  value={
                                                    typeof filter.value ===
                                                    'string'
                                                      ? filter.value
                                                      : filter.value.path
                                                  }
                                                  onChange={(e) =>
                                                    updateFilter(
                                                      index,
                                                      filterIndex,
                                                      'value',
                                                      e.target.value
                                                    )
                                                  }
                                                  placeholder={
                                                    filterType?.placeholder ||
                                                    'Enter value'
                                                  }
                                                  className="flex-1"
                                                />
                                              )}

                                              <Button
                                                type="button"
                                                variant="ghost"
                                                size="icon"
                                                onClick={() =>
                                                  removeFilter(
                                                    index,
                                                    filterIndex
                                                  )
                                                }
                                              >
                                                <Trash2 className="h-4 w-4" />
                                              </Button>
                                            </div>
                                            {isJsonType && (
                                              <p className="text-xs text-muted-foreground ml-[196px]">
                                                {hasError
                                                  ? ''
                                                  : 'Use JSON format with string values only'}
                                              </p>
                                            )}
                                          </div>
                                        )
                                      }
                                    )}
                                  </div>
                                ) : (
                                  <p className="text-sm text-muted-foreground">
                                    No filters added yet
                                  </p>
                                )}
                              </div>
                            )}
                          </div>
                        </div>
                      </CardContent>
                    </Card>
                  ))}
                </div>

                <div className="flex gap-2 pt-4">
                  <Button
                    type="button"
                    variant="outline"
                    onClick={onCancel}
                    disabled={isSubmitting}
                    className="flex-1"
                  >
                    Cancel
                  </Button>
                  <Button
                    type="submit"
                    disabled={isSubmitting}
                    className="flex-1"
                  >
                    {isSubmitting ? 'Saving...' : submitLabel}
                  </Button>
                </div>
              </form>
            </CardContent>
          </Card>
        </div>

        {/* Preview Section */}
        <div className="space-y-6 lg:sticky lg:top-6 lg:h-fit">
          <Card>
            <CardHeader>
              <CardTitle>Funnel Preview</CardTitle>
              <CardDescription>
                {previewMutation.isPending
                  ? 'Loading preview...'
                  : 'Visual representation based on your analytics data'}
              </CardDescription>
            </CardHeader>
            <CardContent className="space-y-4">
              {formData.steps.length === 0 ? (
                <div className="text-center py-8 text-muted-foreground">
                  <p>Add steps to see the funnel preview</p>
                </div>
              ) : previewMutation.isPending ? (
                <div className="space-y-4">
                  <div className="grid grid-cols-3 gap-3">
                    {Array.from({ length: 3 }).map((_, i) => (
                      <Card key={i}>
                        <CardContent className="pt-4">
                          <div className="text-center space-y-2">
                            <Skeleton className="h-4 w-4 mx-auto" />
                            <Skeleton className="h-8 w-16 mx-auto" />
                            <Skeleton className="h-3 w-20 mx-auto" />
                          </div>
                        </CardContent>
                      </Card>
                    ))}
                  </div>
                  <div className="space-y-3">
                    {Array.from({ length: formData.steps.length }).map(
                      (_, i) => (
                        <Skeleton key={i} className="h-20 w-full" />
                      )
                    )}
                  </div>
                </div>
              ) : previewMutation.data ? (
                <>
                  {/* Summary Cards */}
                  <div className="grid grid-cols-3 gap-3">
                    <Card>
                      <CardContent className="pt-4">
                        <div className="text-center">
                          <Users className="h-4 w-4 mx-auto mb-2 text-muted-foreground" />
                          <div className="text-2xl font-bold">
                            {previewMutation.data.total_entries.toLocaleString()}
                          </div>
                          <div className="text-xs text-muted-foreground">
                            Total Entries
                          </div>
                        </div>
                      </CardContent>
                    </Card>
                    <Card>
                      <CardContent className="pt-4">
                        <div className="text-center">
                          <TrendingDown className="h-4 w-4 mx-auto mb-2 text-muted-foreground" />
                          <div className="text-2xl font-bold">
                            {previewMutation.data.step_conversions.length > 0
                              ? previewMutation.data.step_conversions[
                                  previewMutation.data.step_conversions.length -
                                    1
                                ].completions.toLocaleString()
                              : 0}
                          </div>
                          <div className="text-xs text-muted-foreground">
                            Completions
                          </div>
                        </div>
                      </CardContent>
                    </Card>
                    <Card>
                      <CardContent className="pt-4">
                        <div className="text-center">
                          <Percent className="h-4 w-4 mx-auto mb-2 text-muted-foreground" />
                          <div className="text-2xl font-bold">
                            {previewMutation.data.overall_conversion_rate.toFixed(
                              1
                            )}
                            %
                          </div>
                          <div className="text-xs text-muted-foreground">
                            Conversion
                          </div>
                        </div>
                      </CardContent>
                    </Card>
                  </div>

                  {/* Funnel Visualization */}
                  <div className="space-y-3 pt-4">
                    {previewMutation.data.step_conversions.map(
                      (stepMetric, index) => {
                        const prevStepMetric =
                          index > 0
                            ? previewMutation.data!.step_conversions[index - 1]
                            : null
                        const dropoff = prevStepMetric
                          ? prevStepMetric.completions - stepMetric.completions
                          : 0
                        const step = formData.steps[index]

                        return (
                          <div key={index} className="space-y-2">
                            {index > 0 && (
                              <div className="flex items-center gap-2 ml-12 text-xs text-muted-foreground">
                                <TrendingDown className="h-3 w-3" />
                                <span>
                                  {dropoff.toLocaleString()} users dropped (
                                  {stepMetric.drop_off_rate.toFixed(1)}%)
                                </span>
                              </div>
                            )}
                            <div className="flex items-start gap-3">
                              <div className="flex-shrink-0 w-8 h-8 bg-primary text-primary-foreground rounded-full flex items-center justify-center text-sm font-semibold">
                                {stepMetric.step_order}
                              </div>
                              <div className="flex-1">
                                <div
                                  className="rounded-lg p-4 transition-all bg-primary border border-primary"
                                  style={{
                                    width: `${Math.max(stepMetric.conversion_rate, 20)}%`,
                                    minWidth: '200px',
                                  }}
                                >
                                  <div className="text-sm font-medium text-primary-foreground">
                                    {stepMetric.step_name}
                                  </div>
                                  <div className="flex items-center justify-between mt-2">
                                    <span className="text-xs text-primary-foreground/80">
                                      {stepMetric.completions.toLocaleString()}{' '}
                                      users
                                    </span>
                                    <span className="text-xs font-semibold text-primary-foreground">
                                      {stepMetric.conversion_rate.toFixed(1)}%
                                    </span>
                                  </div>
                                  {step?.event_filter &&
                                    step.event_filter.length > 0 && (
                                      <div className="mt-2 pt-2 border-t border-primary-foreground/20">
                                        <div className="flex flex-wrap gap-1">
                                          {step.event_filter.map(
                                            (filter, filterIndex) => {
                                              const filterType =
                                                FILTER_TYPES.find(
                                                  (ft) =>
                                                    ft.value === filter.type
                                                )
                                              const filterValue =
                                                filter.type === 'custom_data'
                                                  ? typeof filter.value ===
                                                    'object'
                                                    ? JSON.stringify(
                                                        filter.value
                                                      )
                                                    : filter.value
                                                  : filter.value

                                              return (
                                                <Badge
                                                  key={filterIndex}
                                                  variant="secondary"
                                                  className="text-xs bg-primary-foreground/20 text-primary-foreground"
                                                >
                                                  {filterType?.label}:{' '}
                                                  {filterValue}
                                                </Badge>
                                              )
                                            }
                                          )}
                                        </div>
                                      </div>
                                    )}
                                </div>
                              </div>
                            </div>
                          </div>
                        )
                      }
                    )}
                  </div>
                </>
              ) : (
                <div className="text-center py-8 space-y-3">
                  <div className="text-muted-foreground">
                    <p>Preview will appear as you configure your funnel</p>
                    <p className="text-sm mt-2">
                      Enter a funnel name and add steps to see preview data
                    </p>
                  </div>
                </div>
              )}
            </CardContent>
          </Card>

          {/* Additional Metrics Card */}
          {previewMutation.data && (
            <Card>
              <CardHeader>
                <CardTitle className="text-base">Additional Metrics</CardTitle>
              </CardHeader>
              <CardContent className="space-y-3">
                <div className="flex items-center gap-3">
                  <Clock className="h-5 w-5 text-muted-foreground" />
                  <div>
                    <p className="text-sm text-muted-foreground">
                      Avg. Completion Time
                    </p>
                    <p className="text-lg font-semibold">
                      {Math.round(
                        previewMutation.data.average_completion_time_seconds /
                          60
                      )}
                      m
                    </p>
                  </div>
                </div>
              </CardContent>
            </Card>
          )}

          {/* Tips Card */}
          <Card>
            <CardHeader>
              <CardTitle className="text-base">Tips</CardTitle>
            </CardHeader>
            <CardContent className="text-sm space-y-2 text-muted-foreground">
              <p>
                • Start with broad entry events and narrow down with filters
              </p>
              <p>• Add 3-7 steps for optimal funnel tracking</p>
              <p>• Use filters to segment users by behavior or attributes</p>
              <p>• Preview shows real data from your analytics events</p>
            </CardContent>
          </Card>
        </div>
      </div>
    </div>
  )
}
