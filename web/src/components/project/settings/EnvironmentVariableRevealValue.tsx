// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { useEffect, useState, useSyncExternalStore } from 'react'
import { useMutation } from '@tanstack/react-query'
import { Eye, EyeOff } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { getEnvVarValue } from '@/lib/resolved-env-vars'
import {
  createValueReveal,
  type ValueRevealState,
} from '@/lib/value-reveal-controller'
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
  // Always mounted; `isSecret` only changes what it renders and whether a
  // fetch is possible. Keyed so a new project, variable, version or secret
  // classification starts hidden with no value.
  return (
    <RevealableValue
      key={`${projectId}:${variableId}:${version}:${isSecret}`}
      projectId={projectId}
      variableId={variableId}
      name={name}
      isSecret={isSecret}
    />
  )
}

function RevealableValue({
  projectId,
  variableId,
  name,
  isSecret,
}: {
  projectId: number
  variableId: number
  name: string
  isSecret: boolean
}) {
  // The app's default mutation error handler reports a failed reveal (using
  // `errorTitle`), so the controller only resets state and never toasts.
  const reveal = useMutation({
    mutationFn: () => {
      // Marked secrets are write-only: never request their value.
      if (isSecret) return Promise.reject(new Error(`${name} is a secret`))
      return getEnvVarValue(projectId, name, variableId)
    },
    meta: { errorTitle: `Failed to reveal ${name}` },
  })
  const [controller] = useState(() =>
    createValueReveal(() => reveal.mutateAsync())
  )
  useEffect(() => () => controller.dispose(), [controller])
  const state = useSyncExternalStore(
    controller.subscribe,
    controller.getState,
    controller.getState
  )
  if (isSecret) {
    return (
      <>
        <dd className="mt-1 font-mono">{MASK}</dd>
        <dd className="text-xs text-muted-foreground">Secret · write-only</dd>
      </>
    )
  }
  return (
    <EnvironmentVariableRevealView
      name={name}
      state={state}
      onReveal={() => void controller.reveal()}
      onHide={() => controller.hide()}
    />
  )
}

export function EnvironmentVariableRevealView({
  name,
  state,
  onReveal,
  onHide,
}: {
  name: string
  state: ValueRevealState
  onReveal: () => void
  onHide: () => void
}) {
  const isVisible = state.phase !== 'hidden'
  return (
    <>
      <dd className="mt-1 flex min-w-0 items-center gap-2">
        {state.phase === 'revealed' ? (
          <EnvironmentVariableValue name={name} value={state.value} />
        ) : (
          <span className="font-mono">
            {state.phase === 'revealing' ? 'Revealing…' : MASK}
          </span>
        )}
        <Button
          variant="ghost"
          size="sm"
          onClick={isVisible ? onHide : onReveal}
          disabled={state.phase === 'revealing'}
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
