import { ServiceTypeParameterResponse } from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
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
import { zodResolver } from '@hookform/resolvers/zod'
import { Loader2 } from 'lucide-react'
import { useEffect, useMemo } from 'react'
import { useForm } from 'react-hook-form'
import * as z from 'zod'

interface DynamicFormProps {
  /**
   * Array of parameter definitions from the API
   */
  parameters: ServiceTypeParameterResponse[]

  /**
   * Callback when form is submitted
   */
  onSubmit: (values: Record<string, string>) => Promise<void> | void

  /**
   * Callback when cancel button is clicked
   */
  onCancel?: () => void

  /**
   * Text for the submit button
   * @default "Submit"
   */
  submitText?: string

  /**
   * Text for the cancel button
   * @default "Cancel"
   */
  cancelText?: string

  /**
   * Whether to show the cancel button
   * @default true
   */
  showCancel?: boolean

  /**
   * Whether the form is in a loading/submitting state
   */
  isSubmitting?: boolean

  /**
   * Initial values for the form fields
   */
  initialValues?: Record<string, string>

  /**
   * Fields to pair together on the same row
   * @default [['host', 'port'], ['username', 'password']]
   */
  pairedFields?: [string, string][]
}

/**
 * Dynamic form component that generates form fields based on JSON schema
 * Supports input fields, password fields, and select dropdowns with choices
 */
export function DynamicForm({
  parameters,
  onSubmit,
  onCancel,
  submitText = 'Submit',
  cancelText = 'Cancel',
  showCancel = true,
  isSubmitting = false,
  initialValues = {},
  pairedFields = [
    ['host', 'port'],
    ['username', 'password'],
  ],
}: DynamicFormProps) {
  // Create form schema dynamically based on parameters
  const formSchema = useMemo(
    () =>
      z.object({
        ...parameters.reduce<Record<string, z.ZodString>>((acc, param) => {
          let schema = z.string()

          if (param.required) {
            schema = schema.min(1, `${param.name} is required`)
          } else {
            schema = schema.optional()
          }

          if (param.validation_pattern) {
            schema = schema.regex(
              new RegExp(param.validation_pattern),
              `Invalid ${param.name} format`
            )
          }

          acc[param.name] = schema
          return acc
        }, {}),
      }),
    [parameters]
  )

  type FormValues = z.infer<typeof formSchema>

  // Calculate default values from parameters and initial values
  const defaultValues = useMemo(() => {
    const defaults: Record<string, string> = {}

    parameters.forEach((param) => {
      if (initialValues[param.name] !== undefined) {
        defaults[param.name] = initialValues[param.name]
      } else if (param.default_value) {
        defaults[param.name] = param.default_value
      } else {
        defaults[param.name] = ''
      }
    })

    return defaults
  }, [parameters, initialValues])

  const form = useForm<FormValues>({
    resolver: zodResolver(formSchema),
    mode: 'onSubmit',
    defaultValues,
  })

  // Update form values when parameters or initial values change
  useEffect(() => {
    form.reset(defaultValues)
  }, [form, defaultValues])

  const handleSubmit = async (values: FormValues) => {
    await onSubmit(values as Record<string, string>)
  }

  // Check if a parameter should be paired with the next one
  const isPairedField = (paramName: string, nextParamName?: string) => {
    if (!nextParamName) return false
    return pairedFields.some(
      ([first, second]) =>
        (first === paramName && second === nextParamName) ||
        (second === paramName && first === nextParamName)
    )
  }

  // Check if this parameter is the second in a pair (should be skipped)
  const isSecondInPair = (paramName: string, index: number) => {
    if (index === 0) return false
    const prevParam = parameters[index - 1]
    return isPairedField(prevParam.name, paramName)
  }

  return (
    <Form {...form}>
      <form onSubmit={form.handleSubmit(handleSubmit)} className="space-y-6">
        {parameters.map((param, index) => {
          const nextParam = parameters[index + 1]
          const shouldPair = isPairedField(param.name, nextParam?.name)
          const isSecond = isSecondInPair(param.name, index)

          // Skip rendering if this is the second field in a pair
          if (isSecond) {
            return null
          }

          // Render paired fields side-by-side
          if (shouldPair && nextParam) {
            return (
              <div key={param.name} className="grid grid-cols-2 gap-4">
                {/* First field */}
                <FormField
                  control={form.control}
                  name={param.name as keyof FormValues}
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>
                        {param.name.charAt(0).toUpperCase() +
                          param.name.slice(1)}
                        {param.required && (
                          <span className="text-destructive ml-1">*</span>
                        )}
                      </FormLabel>
                      <FormControl>
                        <Input
                          {...field}
                          type={param.encrypted ? 'password' : 'text'}
                          placeholder={param.default_value || undefined}
                        />
                      </FormControl>
                      {param.description && (
                        <FormDescription>{param.description}</FormDescription>
                      )}
                      <FormMessage />
                    </FormItem>
                  )}
                />

                {/* Second field */}
                <FormField
                  control={form.control}
                  name={nextParam.name as keyof FormValues}
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>
                        {nextParam.name.charAt(0).toUpperCase() +
                          nextParam.name.slice(1)}
                        {nextParam.required && (
                          <span className="text-destructive ml-1">*</span>
                        )}
                      </FormLabel>
                      <FormControl>
                        <Input
                          {...field}
                          type={nextParam.encrypted ? 'password' : 'text'}
                          placeholder={nextParam.default_value || undefined}
                        />
                      </FormControl>
                      {nextParam.description && (
                        <FormDescription>
                          {nextParam.description}
                        </FormDescription>
                      )}
                      <FormMessage />
                    </FormItem>
                  )}
                />
              </div>
            )
          }

          // Render single field
          return (
            <FormField
              key={param.name}
              control={form.control}
              name={param.name as keyof FormValues}
              render={({ field }) => (
                <FormItem>
                  <FormLabel>
                    {param.name.charAt(0).toUpperCase() + param.name.slice(1)}
                    {param.required && (
                      <span className="text-destructive ml-1">*</span>
                    )}
                  </FormLabel>
                  <FormControl>
                    {param.choices && param.choices.length > 0 ? (
                      // Render Select for fields with choices
                      <Select
                        onValueChange={field.onChange}
                        value={
                          (field.value as string) ||
                          param.default_value ||
                          undefined
                        }
                      >
                        <SelectTrigger>
                          <SelectValue
                            placeholder={
                              param.default_value
                                ? `Default: ${param.default_value}`
                                : `Select ${param.name}`
                            }
                          />
                        </SelectTrigger>
                        <SelectContent>
                          {param.choices.map((choice) => (
                            <SelectItem key={choice} value={choice}>
                              {choice}
                            </SelectItem>
                          ))}
                        </SelectContent>
                      </Select>
                    ) : (
                      // Render Input for fields without choices
                      <Input
                        {...field}
                        type={param.encrypted ? 'password' : 'text'}
                        placeholder={param.default_value || undefined}
                      />
                    )}
                  </FormControl>
                  {param.description && (
                    <FormDescription>{param.description}</FormDescription>
                  )}
                  <FormMessage />
                </FormItem>
              )}
            />
          )
        })}

        {/* Action Buttons */}
        <div className="flex justify-end space-x-3 pt-6">
          {showCancel && onCancel && (
            <Button
              type="button"
              variant="outline"
              onClick={onCancel}
              disabled={isSubmitting}
            >
              {cancelText}
            </Button>
          )}
          <Button type="submit" disabled={isSubmitting}>
            {isSubmitting ? (
              <>
                <Loader2 className="h-4 w-4 mr-2 animate-spin" />
                Submitting...
              </>
            ) : (
              submitText
            )}
          </Button>
        </div>
      </form>
    </Form>
  )
}
