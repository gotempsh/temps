// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Info } from 'lucide-react'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'

interface VendorBaseUrlHintProps {
  /** Whether a vendor provider has a custom base URL typed in. */
  open: boolean
  /** Display name of the vendor provider being added, e.g. "OpenAI". */
  providerName: string
  /** Switch the form to an OpenAI-compatible provider, keeping the input. */
  onSwitch: () => void
  disabled?: boolean
}

/**
 * Shown when a vendor provider gets a custom base URL. The key is verified
 * with that vendor's own model names, so another vendor's OpenAI-protocol
 * API (DeepSeek, Groq, ...) fails there with an unknown-model error; this
 * says so before submitting and offers the provider that does fit.
 */
export function VendorBaseUrlHint({
  open,
  providerName,
  onSwitch,
  disabled,
}: VendorBaseUrlHintProps) {
  if (!open) return null
  // A note, not an alert: it is advice shown while typing, and keeping it out
  // of the alert role leaves the dialog's alert to mean "the save failed".
  return (
    <Alert role="note">
      <Info className="h-4 w-4" />
      <AlertDescription className="grid gap-2">
        <span>
          This key will be verified with {providerName}&apos;s own models. For
          another API that speaks the OpenAI protocol (DeepSeek, Groq, Together,
          vLLM…), add an OpenAI-compatible provider: it lists the server&apos;s
          models instead.
        </span>
        <Button
          type="button"
          variant="outline"
          size="sm"
          className="w-full sm:w-fit"
          onClick={onSwitch}
          disabled={disabled}
        >
          Switch to OpenAI-compatible
        </Button>
      </AlertDescription>
    </Alert>
  )
}
