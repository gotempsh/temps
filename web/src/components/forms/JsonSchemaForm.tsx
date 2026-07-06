import { Button } from '@/components/ui/button'
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from '@/components/ui/collapsible'
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
import { customAlphabet } from 'nanoid'
import {
  ChevronDown,
  Eye,
  EyeOff,
  Loader2,
  Sparkles,
} from 'lucide-react'
import { type ReactNode, useEffect, useMemo, useState } from 'react'
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

type FieldGroup = 'basic' | 'connection' | 'credentials' | 'advanced'

interface FieldHint {
  group: FieldGroup
  /** Hide by default when the service is managed by Temps (container-backed). */
  hiddenWhenManaged?: boolean
}

interface JsonSchemaFormProps {
  schema: JsonSchema
  onSubmit: (
    values: Record<string, string | null | number>
  ) => Promise<void> | void
  onCancel?: () => void
  submitText?: string
  cancelText?: string
  showCancel?: boolean
  isSubmitting?: boolean
  initialValues?: Record<string, string | null>
  pairedFields?: [string, string][]
  hiddenFields?: string[]
  /**
   * Service type for looking up field grouping hints. When unset, all fields
   * render inline (pre-grouping behaviour) — safe fallback for unknown types.
   */
  serviceType?: string
  /**
   * When true, fields marked `hiddenWhenManaged` are moved into the Advanced
   * group. Use for Temps-provisioned services where host/port are outputs.
   */
  managedByTemps?: boolean
  /**
   * Field values owned by a parent (preset). These are hidden from the form
   * UI and merged into the submitted payload with precedence over user input.
   * A value of `undefined` means "preset owns this field but has no value
   * yet" — the field stays hidden and is omitted from submission.
   */
  fieldOverrides?: Record<string, string | null | undefined>
  /**
   * Explicit list of field names the parent presets own. Used to hide fields
   * even when their current override value is undefined/empty.
   */
  presetOwnedFields?: string[]
}

/** Acronyms that should stay uppercase when humanizing snake_case field names. */
const ACRONYMS = new Set([
  'api',
  'db',
  'id',
  'ip',
  'jwt',
  'rds',
  'sql',
  'ssl',
  'tls',
  'ttl',
  'url',
  'uri',
])

function humanizeLabel(fieldName: string): string {
  return fieldName
    .split('_')
    .map((word) => {
      if (ACRONYMS.has(word.toLowerCase())) return word.toUpperCase()
      return word.charAt(0).toUpperCase() + word.slice(1)
    })
    .join(' ')
}

/** Per-service field classification. Fallback for unknown keys: 'advanced'. */
const FIELD_HINTS: Record<string, Record<string, FieldHint>> = {
  postgres: {
    host: { group: 'connection', hiddenWhenManaged: true },
    port: { group: 'connection', hiddenWhenManaged: true },
    database: { group: 'credentials' },
    username: { group: 'credentials' },
    password: { group: 'credentials' },
    max_connections: { group: 'advanced' },
    ssl_mode: { group: 'advanced' },
    docker_image: { group: 'advanced' },
  },
  mariadb: {
    host: { group: 'connection', hiddenWhenManaged: true },
    port: { group: 'connection', hiddenWhenManaged: true },
    database: { group: 'credentials' },
    username: { group: 'credentials' },
    password: { group: 'credentials' },
    root_password: { group: 'credentials' },
    size_profile: { group: 'basic' },
    docker_image: { group: 'advanced' },
  },
  redis: {
    host: { group: 'connection', hiddenWhenManaged: true },
    port: { group: 'connection', hiddenWhenManaged: true },
    password: { group: 'credentials' },
    database: { group: 'advanced' },
    docker_image: { group: 'advanced' },
    max_memory: { group: 'advanced' },
    max_memory_policy: { group: 'advanced' },
  },
  mongodb: {
    host: { group: 'connection', hiddenWhenManaged: true },
    port: { group: 'connection', hiddenWhenManaged: true },
    database: { group: 'credentials' },
    username: { group: 'credentials' },
    password: { group: 'credentials' },
    auth_source: { group: 'advanced' },
    replica_set: { group: 'advanced' },
    docker_image: { group: 'advanced' },
  },
  s3: {
    endpoint: { group: 'connection' },
    region: { group: 'connection' },
    bucket: { group: 'connection' },
    access_key_id: { group: 'credentials' },
    secret_access_key: { group: 'credentials' },
    force_path_style: { group: 'advanced' },
  },
  rustfs: {
    host: { group: 'connection', hiddenWhenManaged: true },
    port: { group: 'connection', hiddenWhenManaged: true },
    bucket: { group: 'connection' },
    access_key_id: { group: 'credentials' },
    secret_access_key: { group: 'credentials' },
    docker_image: { group: 'advanced' },
  },
  minio: {
    host: { group: 'connection', hiddenWhenManaged: true },
    port: { group: 'connection', hiddenWhenManaged: true },
    bucket: { group: 'connection' },
    access_key_id: { group: 'credentials' },
    secret_access_key: { group: 'credentials' },
    docker_image: { group: 'advanced' },
  },
}

const GROUP_LABELS: Record<FieldGroup, string> = {
  basic: 'Basic',
  connection: 'Connection',
  credentials: 'Credentials',
  advanced: 'Advanced',
}

/** nanoid alphabet for passwords: letters + digits, no ambiguous chars. */
const PASSWORD_ALPHABET =
  'ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnpqrstuvwxyz23456789'
const generatePassword = customAlphabet(PASSWORD_ALPHABET, 24)

function resolveGroup(
  fieldName: string,
  serviceType: string | undefined,
  managedByTemps: boolean
): FieldGroup {
  if (!serviceType) return 'basic'
  const hint = FIELD_HINTS[serviceType]?.[fieldName]
  if (!hint) return 'advanced'
  if (hint.hiddenWhenManaged && managedByTemps) return 'advanced'
  return hint.group
}

/**
 * Form component that generates fields based on JSON Schema.
 * Groups fields into Basic → Connection → Credentials → Advanced when a
 * serviceType is supplied; otherwise renders flat (legacy behaviour).
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
    ['access_key_id', 'secret_access_key'],
  ],
  hiddenFields = [],
  serviceType,
  managedByTemps = false,
  fieldOverrides,
  presetOwnedFields,
}: JsonSchemaFormProps) {
  const ownedFields = useMemo(
    () => presetOwnedFields ?? Object.keys(fieldOverrides ?? {}),
    [presetOwnedFields, fieldOverrides]
  )
  const effectiveHiddenFields = useMemo(
    () => [...hiddenFields, ...ownedFields],
    [hiddenFields, ownedFields]
  )
  const [advancedOpen, setAdvancedOpen] = useState(false)
  const [revealedPasswords, setRevealedPasswords] = useState<
    Record<string, boolean>
  >({})

  const propertyNames = useMemo(
    () =>
      Object.keys(schema.properties).filter(
        (name) => !effectiveHiddenFields.includes(name)
      ),
    [schema.properties, effectiveHiddenFields]
  )

  const formSchema = useMemo(() => {
    const zodFields: Record<string, z.ZodTypeAny> = {}

    Object.entries(schema.properties).forEach(([key, prop]) => {
      const isRequired = schema.required?.includes(key)
      const types = Array.isArray(prop.type) ? prop.type : [prop.type]
      const isNullable = types.includes('null')
      const isString = types.includes('string')

      let fieldSchema: z.ZodTypeAny

      if (isString && isNullable) {
        fieldSchema = z.string().optional()
      } else if (isString) {
        fieldSchema = z.string()
      } else {
        fieldSchema = z.string()
      }

      if (isRequired && !isNullable) {
        fieldSchema = (fieldSchema as z.ZodString).min(
          1,
          `${humanizeLabel(key)} is required`
        )
      }

      zodFields[key] = fieldSchema
    })

    return z.object(zodFields)
  }, [schema])

  type FormValues = z.infer<typeof formSchema>

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

  const defaultValuesString = JSON.stringify(defaultValues)
  useEffect(() => {
    const values = JSON.parse(defaultValuesString)
    form.reset(values)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [defaultValuesString])

  if (!schema.properties || Object.keys(schema.properties).length === 0) {
    return null
  }

  const handleSubmit = async (values: FormValues) => {
    const cleanedValues: Record<string, string | null | number> = {}

    Object.entries(values).forEach(([key, value]) => {
      if (hiddenFields.includes(key)) return
      // Skip override-controlled fields — they'll be merged in below.
      if (ownedFields.includes(key)) return

      const prop = schema.properties[key]
      const types = Array.isArray(prop.type) ? prop.type : [prop.type]
      const isNullable = types.includes('null')
      const isInteger = types.includes('integer')

      if (value === '' && isNullable) {
        cleanedValues[key] = null
      } else if (isInteger && value !== '') {
        const numValue = Number(value)
        cleanedValues[key] = isNaN(numValue) ? 0 : numValue
      } else {
        cleanedValues[key] = value as string
      }
    })

    if (fieldOverrides) {
      for (const [key, val] of Object.entries(fieldOverrides)) {
        // undefined → preset owns this field but has no value — omit entirely.
        if (val === undefined) continue
        cleanedValues[key] = val
      }
    }

    await onSubmit(cleanedValues)
  }

  const isPairedField = (fieldName: string, nextFieldName?: string) => {
    if (!nextFieldName) return false
    return pairedFields.some(
      ([first, second]) =>
        (first === fieldName && second === nextFieldName) ||
        (second === fieldName && first === nextFieldName)
    )
  }

  const isPasswordField = (fieldName: string) => {
    const lower = fieldName.toLowerCase()
    return (
      lower.includes('password') ||
      lower.includes('secret') ||
      lower === 'access_key_id'
    )
  }

  const isSecretField = (fieldName: string) => {
    const lower = fieldName.toLowerCase()
    return lower.includes('password') || lower.includes('secret')
  }

  const renderPasswordField = (
    fieldName: string,
    property: JsonSchemaProperty,
    isRequired: boolean
  ) => {
    const revealed = revealedPasswords[fieldName] ?? false
    const isGeneratable =
      !isRequired && fieldName.toLowerCase() === 'password'

    return (
      <FormField
        key={fieldName}
        control={form.control}
        name={fieldName as keyof FormValues}
        render={({ field }) => (
          <FormItem>
            <div className="flex items-center justify-between gap-2">
              <FormLabel>
                {humanizeLabel(fieldName)}
                {isRequired && (
                  <span className="text-destructive ml-1">*</span>
                )}
              </FormLabel>
              {isGeneratable && (
                <Button
                  type="button"
                  variant="ghost"
                  size="sm"
                  className="h-6 gap-1 text-xs text-muted-foreground hover:text-foreground"
                  onClick={() => {
                    field.onChange(generatePassword())
                    setRevealedPasswords((s) => ({ ...s, [fieldName]: true }))
                  }}
                >
                  <Sparkles className="h-3 w-3" />
                  Generate
                </Button>
              )}
            </div>
            <FormControl>
              <div className="relative">
                <Input
                  {...field}
                  value={(field.value as string) || ''}
                  type={revealed ? 'text' : 'password'}
                  autoComplete="off"
                  className="pr-10"
                  placeholder={
                    isGeneratable
                      ? 'Leave empty to auto-generate'
                      : property.examples?.[0] ||
                        (property.default
                          ? String(property.default)
                          : undefined)
                  }
                />
                <button
                  type="button"
                  tabIndex={-1}
                  onClick={() =>
                    setRevealedPasswords((s) => ({
                      ...s,
                      [fieldName]: !revealed,
                    }))
                  }
                  className="absolute right-2 top-1/2 -translate-y-1/2 rounded p-1 text-muted-foreground hover:text-foreground"
                  aria-label={revealed ? 'Hide value' : 'Show value'}
                >
                  {revealed ? (
                    <EyeOff className="h-4 w-4" />
                  ) : (
                    <Eye className="h-4 w-4" />
                  )}
                </button>
              </div>
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

  const renderField = (fieldName: string, property: JsonSchemaProperty) => {
    const isRequired = schema.required?.includes(fieldName) ?? false
    const hasChoices = property.enum && property.enum.length > 0
    const types = Array.isArray(property.type) ? property.type : [property.type]
    const isInteger = types.includes('integer')

    if (isSecretField(fieldName)) {
      return renderPasswordField(fieldName, property, isRequired)
    }

    return (
      <FormField
        key={fieldName}
        control={form.control}
        name={fieldName as keyof FormValues}
        render={({ field }) => (
          <FormItem>
            <FormLabel>
              {humanizeLabel(fieldName)}
              {isRequired && <span className="text-destructive ml-1">*</span>}
            </FormLabel>
            <FormControl>
              {hasChoices ? (
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
                          : `Select ${humanizeLabel(fieldName).toLowerCase()}`
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
              ) : isPasswordField(fieldName) ? (
                // access_key_id: sensitive but not secret-masked — treat as text.
                <Input
                  {...field}
                  value={(field.value as string) || ''}
                  type="text"
                  autoComplete="off"
                  placeholder={
                    property.examples?.[0] ||
                    (property.default ? String(property.default) : undefined)
                  }
                />
              ) : (
                <Input
                  {...field}
                  value={(field.value as string) || ''}
                  type={isInteger ? 'number' : 'text'}
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

  // Render a list of field names, respecting pair-into-row behaviour.
  const renderFieldList = (names: string[]) => {
    const elements: ReactNode[] = []
    for (let i = 0; i < names.length; i++) {
      const fieldName = names[i]
      const nextFieldName = names[i + 1]
      const property = schema.properties[fieldName]
      if (!property) continue

      if (nextFieldName && isPairedField(fieldName, nextFieldName)) {
        const nextProperty = schema.properties[nextFieldName]
        elements.push(
          <div key={fieldName} className="grid grid-cols-1 sm:grid-cols-2 gap-4">
            {renderField(fieldName, property)}
            {renderField(nextFieldName, nextProperty)}
          </div>
        )
        i++ // skip the paired field
        continue
      }

      elements.push(
        <div key={fieldName}>{renderField(fieldName, property)}</div>
      )
    }
    return elements
  }

  // Partition fields into groups (only active when serviceType is provided).
  const grouped = useMemo(() => {
    const buckets: Record<FieldGroup, string[]> = {
      basic: [],
      connection: [],
      credentials: [],
      advanced: [],
    }
    propertyNames.forEach((name) => {
      const group = resolveGroup(name, serviceType, managedByTemps)
      buckets[group].push(name)
    })
    return buckets
  }, [propertyNames, serviceType, managedByTemps])

  const useGrouping = !!serviceType
  const hasAdvanced = grouped.advanced.length > 0
  const nonAdvancedGroups: FieldGroup[] = ['basic', 'connection', 'credentials']

  return (
    <Form {...form}>
      <form onSubmit={form.handleSubmit(handleSubmit)} className="space-y-6">
        {useGrouping ? (
          <>
            {nonAdvancedGroups.map((group) => {
              const names = grouped[group]
              if (names.length === 0) return null
              return (
                <section key={group} className="space-y-4">
                  {/* Only show the group header when there are multiple visible groups */}
                  {nonAdvancedGroups.filter((g) => grouped[g].length > 0)
                    .length > 1 && (
                    <h3 className="text-sm font-medium text-muted-foreground">
                      {GROUP_LABELS[group]}
                    </h3>
                  )}
                  <div className="space-y-6">{renderFieldList(names)}</div>
                </section>
              )
            })}

            {hasAdvanced && (
              <Collapsible open={advancedOpen} onOpenChange={setAdvancedOpen}>
                <CollapsibleTrigger asChild>
                  <button
                    type="button"
                    className="flex w-full items-center justify-between rounded-md border bg-muted/30 px-4 py-3 text-sm font-medium hover:bg-muted/50 transition-colors"
                  >
                    <span className="flex items-center gap-2">
                      Advanced configuration
                      <span className="text-xs font-normal text-muted-foreground">
                        {grouped.advanced.length} field
                        {grouped.advanced.length === 1 ? '' : 's'}
                      </span>
                    </span>
                    <ChevronDown
                      className={`h-4 w-4 transition-transform ${
                        advancedOpen ? 'rotate-180' : ''
                      }`}
                    />
                  </button>
                </CollapsibleTrigger>
                <CollapsibleContent className="pt-4">
                  <div className="space-y-6 border-l-2 border-muted pl-4">
                    {renderFieldList(grouped.advanced)}
                  </div>
                </CollapsibleContent>
              </Collapsible>
            )}
          </>
        ) : (
          <div className="space-y-6">{renderFieldList(propertyNames)}</div>
        )}

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
