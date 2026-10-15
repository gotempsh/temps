// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { useEffect, useRef, useState } from 'react'
import { useMutation } from '@tanstack/react-query'
import { Eye, EyeOff } from 'lucide-react'
import { toast } from 'sonner'
import { Button } from '@/components/ui/button'
import {
  createCredentialRevealGuard,
  credentialValueForScope,
  type ScopedCredentialValue,
} from '@/lib/credential-reveal-state'
import { getEnvVarValue } from '@/lib/resolved-env-vars'
import { EnvironmentVariableValue } from './EnvironmentVariableValue'

const MASK = '••••••••••••'

/**
 * Value display for a single variable. Mirrors the list row: regular values are
 * revealed on demand through the audited value endpoint and rendered with the
 * shared `EnvironmentVariableValue`; marked secrets stay masked and write-only.
 */
export function EnvironmentVariableRevealValue({
  projectId,
  variableId,
  name,
  isSecret,
  version,
}: {
  projectId: number
  variableId: number
  name: string
  isSecret: boolean
  /** Changes whenever the stored value changes, so stale plaintext is dropped. */
  version: string
}) {
  const [isVisible, setIsVisible] = useState(false)
  const [revealed, setRevealed] = useState<ScopedCredentialValue | undefined>()
  const scope = `${projectId}:${variableId}:${version}`
  const guard = useRef(createCredentialRevealGuard())

  useEffect(() => {
    const next = createCredentialRevealGuard()
    guard.current = next
    // Drop plaintext whenever the variable changes identity or version.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setRevealed(undefined)
    setIsVisible(false)
    return () => next.invalidate()
  }, [scope])

  const reveal = useMutation({
    mutationFn: () => getEnvVarValue(projectId, name, variableId),
    meta: { errorTitle: `Failed to reveal ${name}` },
  })

  if (isSecret) {
    return (
      <>
        <dd className="mt-1 font-mono">{MASK}</dd>
        <dd className="text-xs text-muted-foreground">Secret · write-only</dd>
      </>
    )
  }

  const value = credentialValueForScope(revealed, scope)

  const hide = () => {
    guard.current.cancel('value')
    setIsVisible(false)
    setRevealed(undefined)
  }

  const show = async () => {
    // Captured once: the guard is swapped when the variable's scope changes.
    const current = guard.current
    setIsVisible(true)
    const request = current.begin('value')
    try {
      const result = await reveal.mutateAsync()
      if (!current.isCurrent('value', request)) return
      setRevealed({ value: result, scope })
    } catch {
      if (current.isCurrent('value', request)) {
        setIsVisible(false)
        toast.error(`Failed to reveal ${name}`)
      }
    } finally {
      current.finish('value', request)
    }
  }

  return (
    <>
      <dd className="mt-1 flex min-w-0 items-center gap-2">
        {isVisible && value !== undefined ? (
          <EnvironmentVariableValue name={name} value={value} />
        ) : (
          <span className="font-mono">
            {isVisible && reveal.isPending ? 'Revealing…' : MASK}
          </span>
        )}
        <Button
          variant="ghost"
          size="sm"
          onClick={() => (isVisible ? hide() : void show())}
          disabled={reveal.isPending}
          aria-label={isVisible ? `Hide ${name}` : `Reveal ${name}`}
          title={isVisible ? 'Hide value' : 'Reveal value (audited)'}
        >
          {isVisible ? (
            <EyeOff className="h-4 w-4" />
          ) : (
            <Eye className="h-4 w-4" />
          )}
        </Button>
      </dd>
      <dd className="text-xs text-muted-foreground">
        {isVisible ? 'Revealed · audited' : 'Reveal to view · audited'}
      </dd>
    </>
  )
}
