import { Button } from '@/components/ui/button'
import {
  listMaskedMcpCredentialFields,
  MASKED_CREDENTIAL_VALUE,
  replaceMcpCredentialValue,
} from '@/lib/mcp-credential-reveal'
import { Eye, EyeOff, KeyRound, Loader2 } from 'lucide-react'
import { useMemo, useState } from 'react'
import { toast } from 'sonner'

interface McpCredentialRevealControlsProps {
  configText: string
  onConfigTextChange: (value: string) => void
  onRevealCredential: (field: string) => Promise<string>
}

export function McpCredentialRevealControls({
  configText,
  onConfigTextChange,
  onRevealCredential,
}: McpCredentialRevealControlsProps) {
  const [revealedFields, setRevealedFields] = useState<Set<string>>(
    () => new Set()
  )
  const [revealingFields, setRevealingFields] = useState<Set<string>>(
    () => new Set()
  )
  const maskedFields = useMemo(
    () => listMaskedMcpCredentialFields(configText),
    [configText]
  )
  const credentialFields = useMemo(
    () =>
      Array.from(new Set([...maskedFields, ...revealedFields])).sort((a, b) =>
        a.localeCompare(b)
      ),
    [maskedFields, revealedFields]
  )

  if (credentialFields.length === 0) return null

  const toggleField = async (field: string) => {
    if (revealedFields.has(field)) {
      onConfigTextChange(
        replaceMcpCredentialValue(configText, field, MASKED_CREDENTIAL_VALUE)
      )
      setRevealedFields((current) => {
        const next = new Set(current)
        next.delete(field)
        return next
      })
      return
    }

    setRevealingFields((current) => new Set(current).add(field))
    try {
      const value = await onRevealCredential(field)
      onConfigTextChange(replaceMcpCredentialValue(configText, field, value))
      setRevealedFields((current) => new Set(current).add(field))
    } catch {
      toast.error(`Failed to reveal ${field}`)
    } finally {
      setRevealingFields((current) => {
        const next = new Set(current)
        next.delete(field)
        return next
      })
    }
  }

  return (
    <div className="rounded-md border bg-muted/30 p-3 space-y-2">
      <div className="flex items-center gap-2 text-sm font-medium">
        <KeyRound className="h-4 w-4" />
        Sensitive configuration
      </div>
      <p className="text-xs text-muted-foreground">
        Values stay masked until you explicitly reveal one.
      </p>
      <div className="space-y-1">
        {credentialFields.map((field) => {
          const isRevealed = revealedFields.has(field)
          const isRevealing = revealingFields.has(field)
          return (
            <div
              key={field}
              className="flex items-center justify-between gap-3 rounded px-2 py-1.5"
            >
              <code className="text-xs break-all">{field}</code>
              <Button
                type="button"
                variant="ghost"
                size="sm"
                onClick={() => toggleField(field)}
                disabled={isRevealing}
                aria-label={isRevealed ? `Hide ${field}` : `Reveal ${field}`}
              >
                {isRevealing ? (
                  <Loader2 className="h-4 w-4 animate-spin" />
                ) : isRevealed ? (
                  <EyeOff className="h-4 w-4" />
                ) : (
                  <Eye className="h-4 w-4" />
                )}
                <span className="ml-2">{isRevealed ? 'Hide' : 'Reveal'}</span>
              </Button>
            </div>
          )
        })}
      </div>
    </div>
  )
}
