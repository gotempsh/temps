import { useEffect, useRef, useState } from 'react'
import { toast } from 'sonner'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Textarea } from '@/components/ui/textarea'
import { cn } from '@/lib/utils'

export interface ParsedEnvVariable {
  key: string
  value: string
  selected: boolean
}

export interface ImportEnvDialogProps {
  /**
   * Whether the dialog is open
   */
  isOpen: boolean
  /**
   * Callback when the dialog's open state changes
   */
  onOpenChange: (open: boolean) => void
  /**
   * Callback when variables are imported
   * @param variables - Array of variables with key, value, and optional environments
   */
  onImport: (
    variables: Array<{
      key: string
      value: string
      environments?: number[]
    }>
  ) => Promise<void>
  /**
   * Optional list of environments to select from
   */
  allEnvironments?: Array<{ id: number; name: string }>
  /**
   * Optional set of existing keys to mark duplicates
   */
  existingKeys?: Set<string>
  /**
   * Title for the dialog
   * @default "Import Environment Variables"
   */
  title?: string
  /**
   * Description for the dialog
   */
  description?: string
  /**
   * Whether to show environment selection
   * @default true if allEnvironments is provided
   */
  showEnvironmentSelection?: boolean
}

/**
 * Shared component for importing environment variables from .env files
 *
 * Supports:
 * - File upload (.env, .env.local, etc.)
 * - Paste content
 * - KEY=VALUE format parsing
 * - Quote removal
 * - Comment and empty line skipping
 * - Duplicate detection
 * - Environment selection (optional)
 *
 * @example
 * ```tsx
 * <ImportEnvDialog
 *   isOpen={isOpen}
 *   onOpenChange={setIsOpen}
 *   onImport={async (variables) => {
 *     // Handle import logic
 *   }}
 *   existingKeys={new Set(['DATABASE_URL'])}
 *   allEnvironments={[{ id: 1, name: 'production' }]}
 * />
 * ```
 */
export function ImportEnvDialog({
  isOpen,
  onOpenChange,
  onImport,
  allEnvironments,
  existingKeys,
  title = 'Import Environment Variables',
  description = 'Upload a .env file or paste its contents to import multiple variables at once.',
  showEnvironmentSelection = !!allEnvironments,
}: ImportEnvDialogProps) {
  const [parsedVariables, setParsedVariables] = useState<ParsedEnvVariable[]>(
    []
  )
  const [selectedEnvironments, setSelectedEnvironments] = useState<number[]>([])
  const [isImporting, setIsImporting] = useState(false)
  const fileInputRef = useRef<HTMLInputElement>(null)
  const [rawContent, setRawContent] = useState('')

  // Default-select all environments when the dialog opens
  useEffect(() => {
    if (
      isOpen &&
      allEnvironments &&
      allEnvironments.length > 0 &&
      selectedEnvironments.length === 0
    ) {
      setSelectedEnvironments(allEnvironments.map((env) => env.id))
    }
  }, [isOpen, allEnvironments, selectedEnvironments.length])

  /**
   * Parse .env file content into key-value pairs
   */
  const parseEnvFile = (content: string): ParsedEnvVariable[] => {
    const lines = content.split('\n')
    const variables: ParsedEnvVariable[] = []

    for (const line of lines) {
      const trimmedLine = line.trim()

      // Skip empty lines and comments
      if (!trimmedLine || trimmedLine.startsWith('#')) {
        continue
      }

      // Parse KEY=VALUE format
      const equalIndex = trimmedLine.indexOf('=')
      if (equalIndex > 0) {
        const key = trimmedLine.substring(0, equalIndex).trim()
        let value = trimmedLine.substring(equalIndex + 1).trim()

        // Remove surrounding quotes if present
        if (
          (value.startsWith('"') && value.endsWith('"')) ||
          (value.startsWith("'") && value.endsWith("'"))
        ) {
          value = value.substring(1, value.length - 1)
        }

        // Check if key already exists
        const alreadyExists = existingKeys?.has(key) ?? false

        variables.push({
          key,
          value,
          selected: !alreadyExists, // Auto-select only new variables
        })
      }
    }

    return variables
  }

  /**
   * Handle file upload
   */
  const handleFileUpload = (event: React.ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0]
    if (!file) return

    const reader = new FileReader()
    reader.onload = (e) => {
      const content = e.target?.result as string
      setRawContent(content)
      const parsed = parseEnvFile(content)
      setParsedVariables(parsed)

      if (parsed.length === 0) {
        toast.error('No valid environment variables found in the file')
      } else {
        toast.success(
          `Found ${parsed.length} environment variable${parsed.length !== 1 ? 's' : ''}`
        )
      }
    }
    reader.readAsText(file)
  }

  /**
   * Handle pasted content parsing
   */
  const handlePasteContent = () => {
    if (!rawContent.trim()) {
      toast.error('Please paste or upload .env file content')
      return
    }
    const parsed = parseEnvFile(rawContent)
    setParsedVariables(parsed)

    if (parsed.length === 0) {
      toast.error('No valid environment variables found')
    } else {
      toast.success(
        `Found ${parsed.length} environment variable${parsed.length !== 1 ? 's' : ''}`
      )
    }
  }

  /**
   * Toggle individual variable selection
   */
  const toggleVariable = (index: number) => {
    setParsedVariables((prev) =>
      prev.map((v, i) => (i === index ? { ...v, selected: !v.selected } : v))
    )
  }

  /**
   * Toggle all variables selection
   */
  const toggleAll = () => {
    const allSelected = parsedVariables.every((v) => v.selected)
    setParsedVariables((prev) =>
      prev.map((v) => ({ ...v, selected: !allSelected }))
    )
  }

  /**
   * Handle import action
   */
  const handleImport = async () => {
    const selectedVars = parsedVariables.filter((v) => v.selected)

    if (selectedVars.length === 0) {
      toast.error('Please select at least one variable to import')
      return
    }

    if (
      showEnvironmentSelection &&
      allEnvironments &&
      selectedEnvironments.length === 0
    ) {
      toast.error('Please select at least one environment')
      return
    }

    setIsImporting(true)
    try {
      await onImport(
        selectedVars.map((v) => ({
          key: v.key,
          value: v.value,
          ...(showEnvironmentSelection && { environments: selectedEnvironments }),
        }))
      )

      // Reset state
      setParsedVariables([])
      setSelectedEnvironments([])
      setRawContent('')
      if (fileInputRef.current) {
        fileInputRef.current.value = ''
      }
      onOpenChange(false)
    } finally {
      setIsImporting(false)
    }
  }

  const selectedCount = parsedVariables.filter((v) => v.selected).length

  return (
    <Dialog open={isOpen} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-3xl max-h-[80vh] flex flex-col">
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
          <DialogDescription>{description}</DialogDescription>
        </DialogHeader>

        <div className="space-y-4 py-4 flex-1 overflow-auto">
          <div className="space-y-2">
            <label className="text-sm font-medium">Upload .env file</label>
            <div className="flex gap-2">
              <Input
                ref={fileInputRef}
                type="file"
                accept=".env,.env.local,.env.production,.env.development,text/plain"
                onChange={handleFileUpload}
                className="flex-1"
              />
            </div>
          </div>

          <div className="relative">
            <div className="absolute inset-0 flex items-center">
              <span className="w-full border-t" />
            </div>
            <div className="relative flex justify-center text-xs uppercase">
              <span className="bg-background px-2 text-muted-foreground">
                Or paste content
              </span>
            </div>
          </div>

          <div className="space-y-2">
            <label className="text-sm font-medium">.env file content</label>
            <Textarea
              placeholder="DATABASE_URL=postgresql://localhost:5432/db&#10;API_KEY=your_api_key_here&#10;NODE_ENV=production"
              value={rawContent}
              onChange={(e) => setRawContent(e.target.value)}
              className="font-mono text-xs min-h-[120px]"
            />
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={handlePasteContent}
              className="w-full"
            >
              Parse Content
            </Button>
          </div>

          {parsedVariables.length > 0 && (
            <>
              <div className="space-y-2">
                <div className="flex items-center justify-between">
                  <label className="text-sm font-medium">
                    Select variables to import ({selectedCount}/
                    {parsedVariables.length})
                  </label>
                  <Button
                    type="button"
                    variant="ghost"
                    size="sm"
                    onClick={toggleAll}
                  >
                    {parsedVariables.every((v) => v.selected)
                      ? 'Deselect All'
                      : 'Select All'}
                  </Button>
                </div>
                <div className="border rounded-md max-h-[250px] overflow-auto">
                  <div className="divide-y">
                    {parsedVariables.map((variable, index) => {
                      const alreadyExists = existingKeys?.has(variable.key) ?? false
                      return (
                        <div
                          key={index}
                          className={cn(
                            'flex items-start gap-3 p-3 hover:bg-muted/50',
                            alreadyExists && 'bg-amber-50 dark:bg-amber-950/20'
                          )}
                        >
                          <Checkbox
                            checked={variable.selected}
                            onCheckedChange={() => toggleVariable(index)}
                            className="mt-1"
                          />
                          <div className="flex-1 space-y-1 min-w-0">
                            <div className="flex items-center gap-2">
                              <p className="font-medium font-mono text-sm">
                                {variable.key}
                              </p>
                              {alreadyExists && (
                                <span className="text-xs px-2 py-0.5 rounded-full bg-amber-100 dark:bg-amber-900 text-amber-800 dark:text-amber-200">
                                  Already exists
                                </span>
                              )}
                            </div>
                            <p className="font-mono text-xs text-muted-foreground truncate">
                              {variable.value}
                            </p>
                          </div>
                        </div>
                      )
                    })}
                  </div>
                </div>
              </div>

              {showEnvironmentSelection && allEnvironments && (
                <div className="space-y-2">
                  <label className="text-sm font-medium">
                    Target Environments
                  </label>
                  <div className="flex flex-wrap gap-2">
                    {allEnvironments.map((env) => (
                      <Button
                        type="button"
                        key={env.id}
                        variant={
                          selectedEnvironments.includes(env.id)
                            ? 'default'
                            : 'outline'
                        }
                        size="sm"
                        onClick={() => {
                          setSelectedEnvironments((prev) =>
                            prev.includes(env.id)
                              ? prev.filter((e) => e !== env.id)
                              : [...prev, env.id]
                          )
                        }}
                      >
                        {env.name}
                      </Button>
                    ))}
                  </div>
                </div>
              )}
            </>
          )}
        </div>

        <DialogFooter>
          <Button
            type="button"
            variant="outline"
            onClick={() => {
              onOpenChange(false)
              setParsedVariables([])
              setSelectedEnvironments([])
              setRawContent('')
              if (fileInputRef.current) {
                fileInputRef.current.value = ''
              }
            }}
          >
            Cancel
          </Button>
          <Button
            type="button"
            onClick={handleImport}
            disabled={
              selectedCount === 0 ||
              (showEnvironmentSelection &&
                allEnvironments &&
                selectedEnvironments.length === 0) ||
              isImporting
            }
          >
            {isImporting
              ? 'Importing...'
              : `Import ${selectedCount} Variable${selectedCount !== 1 ? 's' : ''}`}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
