// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { Eye, EyeOff, KeyRound, Loader2, Sparkles, Trash2 } from 'lucide-react'
import { toast } from 'sonner'

import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { CopyButton } from '@/components/ui/copy-button'
import { Input } from '@/components/ui/input'

import {
  clearPreviewPasswordMutation,
  setPreviewPasswordMutation,
} from '@/api/client/@tanstack/react-query.gen'

// Matches the backend's `preview_password::MIN_PASSWORD_LEN`. If that
// constant changes, bump this in lockstep — the server is the source of
// truth, but matching here means bad input never reaches the network.
const MIN_PASSWORD_LEN = 8
const MAX_PASSWORD_LEN = 256

// Length of passwords minted by the "Generate" button. 24 chars from a
// 64-symbol alphabet ≈ 144 bits of entropy — well above any realistic
// brute-force threshold once rate-limiting and argon2 are factored in.
const GENERATED_PASSWORD_LEN = 24

/**
 * Draw a random password from a URL-safe 64-symbol alphabet. Uses
 * `crypto.getRandomValues` with rejection sampling so every symbol is
 * equally likely — `x % 64` would bias on a 256-entry byte stream, but
 * 256 is an exact multiple of 64, so the modulo is uniform here.
 */
function generatePassword(length: number): string {
  const alphabet =
    'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_'
  const buf = new Uint8Array(length)
  crypto.getRandomValues(buf)
  let out = ''
  for (let i = 0; i < length; i++) {
    out += alphabet[buf[i] % alphabet.length]
  }
  return out
}

interface Props {
  sandboxId: string
  /**
   * Last-4 hint from the latest `SandboxResponse`. Undefined when no
   * password is currently configured.
   */
  hint: string | undefined
  /**
   * True for destroyed sandboxes — disables every control so a dead
   * sandbox can't accidentally mutate password state.
   */
  disabled?: boolean
}

/**
 * Compact password-wall control for a standalone sandbox. Mirrors the
 * workspace's `SessionPreviewCard` password section but accepts a
 * user-supplied plaintext (the backend doesn't generate one). The server
 * only ever returns the last-4 hint — the plaintext never round-trips.
 */
export function SandboxPreviewPasswordCard({
  sandboxId,
  hint,
  disabled = false,
}: Props) {
  const queryClient = useQueryClient()
  const [value, setValue] = useState('')
  const [confirm, setConfirm] = useState('')
  const [show, setShow] = useState(false)

  const invalidate = () => {
    queryClient.invalidateQueries({ queryKey: ['sandbox', sandboxId] })
    queryClient.invalidateQueries({ queryKey: ['sandboxes'] })
  }

  const setMutation = useMutation({
    ...setPreviewPasswordMutation(),
    meta: { errorTitle: 'Failed to set preview password' },
    onSuccess: (resp) => {
      invalidate()
      setValue('')
      setConfirm('')
      setShow(false)
      toast.success(
        `Preview password ${hint ? 'rotated' : 'set'} · ends in …${resp.preview_password_hint}`,
      )
    },
  })

  const clearMutation = useMutation({
    ...clearPreviewPasswordMutation(),
    meta: { errorTitle: 'Failed to clear preview password' },
    onSuccess: () => {
      invalidate()
      toast.success('Preview password removed')
    },
  })

  const trimmed = value
  const confirmTrimmed = confirm
  const lengthOk =
    trimmed.length >= MIN_PASSWORD_LEN && trimmed.length <= MAX_PASSWORD_LEN
  const confirmMatches = trimmed === confirmTrimmed
  const canSubmit =
    !disabled && !setMutation.isPending && lengthOk && confirmMatches

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault()
    if (!canSubmit) return
    setMutation.mutate({
      path: { id: sandboxId },
      body: { password: trimmed },
    })
  }

  const handleGenerate = () => {
    const next = generatePassword(GENERATED_PASSWORD_LEN)
    setValue(next)
    setConfirm(next)
    // Reveal so the user can actually see what they're about to submit —
    // hiding a freshly-generated password the user never typed would be
    // pure friction.
    setShow(true)
  }

  return (
    <Card>
      <CardHeader className="pb-3">
        <div className="flex items-center justify-between gap-2">
          <CardTitle className="text-sm font-medium flex items-center gap-2">
            <KeyRound className="h-4 w-4" />
            Preview access
          </CardTitle>
          {hint && !disabled && (
            <Button
              size="sm"
              variant="ghost"
              onClick={() => clearMutation.mutate({ path: { id: sandboxId } })}
              disabled={clearMutation.isPending}
              className="text-destructive hover:text-destructive h-7"
              title="Remove password protection"
            >
              {clearMutation.isPending ? (
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
              ) : (
                <Trash2 className="h-3.5 w-3.5" />
              )}
            </Button>
          )}
        </div>
      </CardHeader>
      <CardContent className="space-y-3">
        <div className="text-xs text-muted-foreground">
          {hint ? (
            <>
              Active password ends in{' '}
              <span className="font-mono">…{hint}</span>. Enter a new one
              below to rotate — existing login cookies are invalidated.
            </>
          ) : (
            <>
              No password set. Preview URLs are reachable by anyone who
              knows the sandbox ID. Set a password to add a login gate on
              every preview port.
            </>
          )}
        </div>

        <form onSubmit={handleSubmit} className="space-y-2">
          <div className="flex items-center gap-2">
            <Input
              type={show ? 'text' : 'password'}
              value={value}
              onChange={(e) => setValue(e.target.value)}
              placeholder={
                hint ? 'New password (8–256 chars)' : 'Password (8–256 chars)'
              }
              minLength={MIN_PASSWORD_LEN}
              maxLength={MAX_PASSWORD_LEN}
              disabled={disabled || setMutation.isPending}
              autoComplete="new-password"
              className="h-9 font-mono text-sm"
            />
            <Button
              type="button"
              variant="ghost"
              size="sm"
              className="h-9 w-9 shrink-0 p-0"
              onClick={() => setShow((v) => !v)}
              tabIndex={-1}
              title={show ? 'Hide' : 'Show'}
            >
              {show ? (
                <EyeOff className="h-3.5 w-3.5" />
              ) : (
                <Eye className="h-3.5 w-3.5" />
              )}
            </Button>
            {/* Show copy only when the input is revealed — copying a
                password we're actively hiding from the user is surprising
                and usually indicates the input holds something the user
                didn't type. */}
            {show && value.length > 0 && (
              <CopyButton value={value} minimal className="h-9 w-9 shrink-0" />
            )}
          </div>
          <div className="flex items-center justify-between gap-2">
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={handleGenerate}
              disabled={disabled || setMutation.isPending}
              className="gap-1.5"
            >
              <Sparkles className="h-3.5 w-3.5" />
              Generate
            </Button>
            <span className="text-[11px] text-muted-foreground">
              {GENERATED_PASSWORD_LEN} chars · copy it before submitting —
              it is never shown again.
            </span>
          </div>
          <Input
            type={show ? 'text' : 'password'}
            value={confirm}
            onChange={(e) => setConfirm(e.target.value)}
            placeholder="Confirm password"
            minLength={MIN_PASSWORD_LEN}
            maxLength={MAX_PASSWORD_LEN}
            disabled={disabled || setMutation.isPending}
            autoComplete="new-password"
            className="h-9 font-mono text-sm"
          />
          {value.length > 0 && !lengthOk && (
            <p className="text-[11px] text-destructive">
              Must be between {MIN_PASSWORD_LEN} and {MAX_PASSWORD_LEN}{' '}
              characters.
            </p>
          )}
          {value.length > 0 && confirm.length > 0 && !confirmMatches && (
            <p className="text-[11px] text-destructive">
              Passwords don't match.
            </p>
          )}
          <div className="flex justify-end">
            <Button
              type="submit"
              size="sm"
              disabled={!canSubmit}
              className="gap-1.5"
            >
              {setMutation.isPending && (
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
              )}
              {hint ? 'Rotate password' : 'Set password'}
            </Button>
          </div>
        </form>
      </CardContent>
    </Card>
  )
}
