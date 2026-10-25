// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState, type ReactNode } from 'react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import {
  Globe,
  KeyRound,
  Loader2,
  ShieldCheck,
  UserRoundCheck,
} from 'lucide-react'
import { toast } from 'sonner'

import type { SandboxPreviewAccess } from '@/api/client'
import {
  getSandboxQueryKey,
  listSandboxesQueryKey,
  updatePreviewAccessMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Textarea } from '@/components/ui/textarea'
import { cn } from '@/lib/utils'

import { MAX_PREVIEW_CIDRS, parseCidrList } from './sandbox-network'

interface Props {
  sandboxId: string
  access: SandboxPreviewAccess
  allowedCidrs: string[]
  /** Whether a preview password is set; `password` needs one. */
  hasPassword: boolean
  /** Destroyed sandboxes freeze the controls. */
  disabled?: boolean
}

const OPTIONS: {
  value: SandboxPreviewAccess
  title: string
  description: string
  icon: ReactNode
}[] = [
  {
    value: 'temps_login',
    title: 'Only you, signed in to Temps',
    description:
      'Visitors sign in to Temps first, and only the sandbox owner gets through. The default.',
    icon: <UserRoundCheck className="h-4 w-4" />,
  },
  {
    value: 'password',
    title: 'Anyone with the password',
    description:
      'Share a preview with people who have no Temps account. Set the password below.',
    icon: <KeyRound className="h-4 w-4" />,
  },
  {
    value: 'public',
    title: 'Anyone with the link',
    description:
      'No sign-in. Use with an IP allow-list unless the page is meant to be public.',
    icon: <Globe className="h-4 w-4" />,
  },
]

/**
 * Who may open the sandbox's preview URLs, and from which networks.
 *
 * The allow-list is checked before the sign-in, in every mode: a visitor
 * from an unlisted address is refused even when signed in.
 */
export function SandboxPreviewAccessCard({
  sandboxId,
  access,
  allowedCidrs,
  hasPassword,
  disabled = false,
}: Props) {
  const queryClient = useQueryClient()
  const [selected, setSelected] = useState<SandboxPreviewAccess>(access)
  const [cidrText, setCidrText] = useState(allowedCidrs.join('\n'))
  // Follow the server when it changes underneath (another tab, a refetch
  // after saving) unless the user is mid-edit.
  const [seen, setSeen] = useState({ access, cidrs: allowedCidrs.join('\n') })
  const serverCidrs = allowedCidrs.join('\n')
  if (seen.access !== access || seen.cidrs !== serverCidrs) {
    if (selected === seen.access) setSelected(access)
    if (cidrText === seen.cidrs) setCidrText(serverCidrs)
    setSeen({ access, cidrs: serverCidrs })
  }

  const update = useMutation({
    ...updatePreviewAccessMutation(),
    meta: { errorTitle: 'Could not change who can open the preview' },
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: getSandboxQueryKey({ path: { id: sandboxId } }),
      })
      queryClient.invalidateQueries({ queryKey: listSandboxesQueryKey() })
      toast.success('Preview access saved. It applies within 30 seconds.')
    },
  })

  const { entries, invalid } = parseCidrList(cidrText)
  const tooMany = entries.length > MAX_PREVIEW_CIDRS
  const needsPassword = selected === 'password' && !hasPassword
  const dirty = selected !== access || entries.join('\n') !== serverCidrs
  const problem = invalid
    ? `"${invalid}" is not an IP address or CIDR`
    : tooMany
      ? `At most ${MAX_PREVIEW_CIDRS} entries`
      : needsPassword
        ? 'Set a preview password below before choosing this.'
        : null
  const canSave = !disabled && dirty && problem === null && !update.isPending

  return (
    <Card>
      <CardHeader className="pb-3">
        <CardTitle className="flex items-center gap-2 text-sm font-medium">
          <ShieldCheck className="h-4 w-4" />
          Who can open previews
        </CardTitle>
      </CardHeader>
      <CardContent className="space-y-4">
        <div
          role="radiogroup"
          aria-label="Who can open previews"
          className="grid grid-cols-1 gap-2 md:grid-cols-3"
        >
          {OPTIONS.map((option) => (
            <button
              key={option.value}
              type="button"
              role="radio"
              aria-checked={selected === option.value}
              disabled={disabled}
              onClick={() => setSelected(option.value)}
              className={cn(
                'flex flex-col gap-1 rounded-lg border p-3 text-left text-sm transition-colors',
                'disabled:cursor-not-allowed disabled:opacity-60',
                selected === option.value
                  ? 'border-primary bg-primary/5'
                  : 'hover:bg-muted/50'
              )}
            >
              <span className="flex items-center gap-2 font-medium">
                {option.icon}
                {option.title}
              </span>
              <span className="text-xs text-muted-foreground">
                {option.description}
              </span>
            </button>
          ))}
        </div>

        <div className="space-y-1.5">
          <label
            htmlFor={`preview-cidrs-${sandboxId}`}
            className="text-sm font-medium"
          >
            Allowed networks
          </label>
          <p className="text-xs text-muted-foreground">
            One IP address or CIDR per line, e.g. 203.0.113.7 or
            198.51.100.0/24. Leave empty to allow any address. Checked before
            the sign-in, whichever option is selected.
          </p>
          <Textarea
            id={`preview-cidrs-${sandboxId}`}
            value={cidrText}
            onChange={(event) => setCidrText(event.target.value)}
            placeholder="Any address"
            rows={3}
            spellCheck={false}
            disabled={disabled}
            className="font-mono text-xs"
            aria-invalid={invalid !== null || tooMany}
          />
        </div>

        {problem && (
          <p role="alert" className="text-xs text-destructive">
            {problem}
          </p>
        )}

        <div className="flex justify-end">
          <Button
            type="button"
            size="sm"
            disabled={!canSave}
            onClick={() =>
              update.mutate({
                path: { id: sandboxId },
                body: { access: selected, allowed_cidrs: entries },
              })
            }
          >
            {update.isPending && (
              <Loader2 className="mr-2 h-4 w-4 animate-spin" />
            )}
            Save access
          </Button>
        </div>
      </CardContent>
    </Card>
  )
}
