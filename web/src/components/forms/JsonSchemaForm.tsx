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

interface JsonSchemaProperty {
  type: string | string[]
  description?: string
  default?: string | null
  examples?: string[]
  enum?: string[]
}

interface JsonSchema {
  type: string
  title?: string
  description?: string
  required?: string[]
  properties: Record<string, JsonSchemaProperty>
}

interface JsonSchemaFormProps {
  /**
   * JSON Schema object defining the form structure
   */
  schema: JsonSchema

  /**
   * Callback when form is submitted
   */
  onSubmit: (
    values: Record<string, string | null | number>
  ) => Promise<void> | void

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
  initialValues?: Record<string, string | null>

  /**
   * Fields to pair together on the same row
   * @default [['host', 'port'], ['username', 'password']]
   */
  pairedFields?: [string, string][]
}

/**
 * Form component that generates fields based on JSON Schema
 * Supports text inputs, password inputs, and select dropdowns
 */
export function JsonSchemaForm({
  schema,
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
}: JsonSchemaFormProps) {
  // Get list of property names in order
  const propertyNames = useMemo(
    () => Object.keys(schema.properties),
    [schema.properties]
  )

  // Create Zod schema from JSON Schema
  const formSchema = useMemo(() => {
    const zodFields: Record<string, z.ZodTypeAny> = {}

    Object.entries(schema.properties).forEach(([key, prop]) => {
      const isRequired = schema.required?.includes(key)
      const types = Array.isArray(prop.type) ? prop.type : [prop.type]
      const isNullable = types.includes('null')
      const isString = types.includes('string')

      let fieldSchema: z.ZodTypeAny

      if (isString && isNullable) {
        // String or null - use optional instead of nullable for forms
        fieldSchema = z.string().optional()
      } else if (isString) {
        // String only
        fieldSchema = z.string()
      } else {
        // Fallback to string
        fieldSchema = z.string()
      }

      if (isRequired && !isNullable) {
        fieldSchema = (fieldSchema as z.ZodString).min(1, `${key} is required`)
      }

      zodFields[key] = fieldSchema
    })

    return z.object(zodFields)
  }, [schema])

  type FormValues = z.infer<typeof formSchema>

  // Calculate default values
  const defaultValues = useMemo(() => {
    const defaults: Record<string, string> = {}

    Object.entries(schema.properties).forEach(([key, prop]) => {
      if (initialValues[key] !== undefined) {
        defaults[key] = initialValues[key] || ''
      } else if (prop.default !== undefined && prop.default !== null) {
        defaults[key] = String(prop.default)
      } else {
        defaults[key] = ''
      }
    })

    return defaults
  }, [schema.properties, initialValues])

  const form = useForm<FormValues>({
    resolver: zodResolver(formSchema),
    mode: 'onSubmit',
    defaultValues,
  })

  // Reset form when defaultValues change (using JSON.stringify for stable comparison)
  const defaultValuesString = JSON.stringify(defaultValues)
  useEffect(() => {
    const values = JSON.parse(defaultValuesString)
    form.reset(values)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [defaultValuesString])

  // Only render form if we have schema properties
  if (!schema.properties || Object.keys(schema.properties).length === 0) {
    return null
  }

  const handleSubmit = async (values: FormValues) => {
    // Convert values to appropriate types based on schema
    const cleanedValues: Record<string, string | null | number> = {}

    Object.entries(values).forEach(([key, value]) => {
      const prop = schema.properties[key]
      const types = Array.isArray(prop.type) ? prop.type : [prop.type]
      const isNullable = types.includes('null')
      const isInteger = types.includes('integer')

      // Handle empty values for nullable fields
      if (value === '' && isNullable) {
        cleanedValues[key] = null
      } else if (isInteger && value !== '') {
        // Convert to number for integer fields
        const numValue = Number(value)
        cleanedValues[key] = isNaN(numValue) ? 0 : numValue
      } else {
        cleanedValues[key] = value as string
      }
    })

    await onSubmit(cleanedValues)
  }

  // Check if a field should be paired with the next one
  const isPairedField = (fieldName: string, nextFieldName?: string) => {
    if (!nextFieldName) return false
    return pairedFields.some(
      ([first, second]) =>
        (first === fieldName && second === nextFieldName) ||
        (second === fieldName && first === nextFieldName)
    )
  }

  // Check if this field is the second in a pair (should be skipped)
  const isSecondInPair = (fieldName: string, index: number) => {
    if (index === 0) return false
    const prevField = propertyNames[index - 1]
    return isPairedField(prevField, fieldName)
  }

  // Determine if field is password type (heuristic)
  const isPasswordField = (fieldName: string) => {
    return fieldName.toLowerCase().includes('password')
  }

  // Render a single field
  const renderField = (fieldName: string, property: JsonSchemaProperty) => {
    const isRequired = schema.required?.includes(fieldName)
    const hasChoices = property.enum && property.enum.length > 0
    const types = Array.isArray(property.type) ? property.type : [property.type]
    const isInteger = types.includes('integer')

    return (
      <FormField
        key={fieldName}
        control={form.control}
        name={fieldName as keyof FormValues}
        render={({ field }) => (
          <FormItem>
            <FormLabel>
              {fieldName.charAt(0).toUpperCase() + fieldName.slice(1)}
              {isRequired && <span className="text-destructive ml-1">*</span>}
            </FormLabel>
            <FormControl>
              {hasChoices ? (
                // Render Select for fields with enum
                <Select
                  onValueChange={field.onChange}
                  value={
                    (field.value as string) || property.default || undefined
                  }
                >
                  <SelectTrigger>
                    <SelectValue
                      placeholder={
                        property.default
                          ? `Default: ${property.default}`
                          : `Select ${fieldName}`
                      }
                    />
                  </SelectTrigger>
                  <SelectContent>
                    {property.enum!.map((choice) => (
                      <SelectItem key={choice} value={choice}>
                        {choice}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              ) : (
                // Render Input for regular fields
                <Input
                  {...field}
                  value={(field.value as string) || ''}
                  type={
                    isPasswordField(fieldName)
                      ? 'password'
                      : isInteger
                        ? 'number'
                        : 'text'
                  }
                  autoComplete="off"
                  placeholder={
                    property.examples?.[0] ||
                    (property.default ? String(property.default) : undefined)
                  }
                />
              )}
            </FormControl>
            {property.description && (
              <FormDescription>{property.description}</FormDescription>
            )}
            <FormMessage />
          </FormItem>
        )}
      />
    )
  }

  return (
    <Form {...form}>
      <form onSubmit={form.handleSubmit(handleSubmit)} className="space-y-6">
        {propertyNames.map((fieldName, index) => {
          const property = schema.properties[fieldName]
          const nextFieldName = propertyNames[index + 1]
          const shouldPair = isPairedField(fieldName, nextFieldName)
          const isSecond = isSecondInPair(fieldName, index)

          // Skip if this is the second field in a pair
          if (isSecond) {
            return null
          }

          // Render paired fields side-by-side
          if (shouldPair && nextFieldName) {
            const nextProperty = schema.properties[nextFieldName]
            return (
              <div key={fieldName} className="grid grid-cols-2 gap-4">
                {renderField(fieldName, property)}
                {renderField(nextFieldName, nextProperty)}
              </div>
            )
          }

          // Render single field
          return renderField(fieldName, property)
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
