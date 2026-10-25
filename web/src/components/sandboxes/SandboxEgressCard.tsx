// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { Globe, ListChecks, Loader2, Network, Plus, Trash2 } from 'lucide-react'
import { toast } from 'sonner'

import type { EgressMode, SandboxEgressPolicy } from '@/api/client'
import {
  getSandboxQueryKey,
  listSandboxesQueryKey,
  setEgressPolicyMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { cn } from '@/lib/utils'

import {
  backendEnforcesRestrictedEgress,
  draftsFromPolicy,
  MAX_EGRESS_RULES,
  ruleProblem,
  rulesFromDrafts,
  type EgressRuleDraft,
} from './sandbox-network'

interface Props {
  sandboxId: string
  policy: SandboxEgressPolicy
  /** The sandbox's backend; only some can enforce `restricted`. */
  backend: string | null | undefined
  /** Destroyed sandboxes freeze the controls. */
  disabled?: boolean
}

const MODES: {
  value: EgressMode
  title: string
  description: string
  icon: typeof Globe
}[] = [
  {
    value: 'internet',
    title: 'Public internet',
    description: 'Any public host on ports 80 and 443. The default.',
    icon: Globe,
  },
  {
    value: 'restricted',
    title: 'Only listed destinations',
    description: 'Hosts and networks you list below, on the ports you choose.',
    icon: ListChecks,
  },
]

function samePolicy(a: SandboxEgressPolicy, b: SandboxEgressPolicy): boolean {
  return JSON.stringify(a) === JSON.stringify(b)
}

/**
 * Where the sandbox may connect to. Private, loopback, link-local and
 * cloud-metadata addresses are refused in every mode, and Temps' own relays
 * (models, tools) keep working whatever is chosen.
 */
export function SandboxEgressCard({
  sandboxId,
  policy,
  backend,
  disabled = false,
}: Props) {
  const queryClient = useQueryClient()
  const mode = policy.mode ?? 'internet'
  const [selected, setSelected] = useState<EgressMode>(mode)
  const [drafts, setDrafts] = useState<EgressRuleDraft[]>(() =>
    draftsFromPolicy(policy)
  )
  const [seen, setSeen] = useState(policy)
  if (!samePolicy(seen, policy)) {
    setSeen(policy)
    setSelected(policy.mode ?? 'internet')
    setDrafts(draftsFromPolicy(policy))
  }

  const save = useMutation({
    ...setEgressPolicyMutation(),
    meta: { errorTitle: 'Could not change where the sandbox can connect' },
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: getSandboxQueryKey({ path: { id: sandboxId } }),
      })
      queryClient.invalidateQueries({ queryKey: listSandboxesQueryKey() })
      toast.success('Outbound network saved')
    },
  })

  const enforceable = backendEnforcesRestrictedEgress(backend)
  const restricted = selected === 'restricted'
  const problems = restricted ? drafts.map(ruleProblem) : []
  const firstProblem = problems.find((problem) => problem !== null) ?? null
  const next: SandboxEgressPolicy = restricted
    ? { mode: 'restricted', rules: firstProblem ? [] : rulesFromDrafts(drafts) }
    : { mode: 'internet', rules: [] }
  const dirty = !samePolicy(next, {
    mode,
    rules: mode === 'internet' ? [] : (policy.rules ?? []),
  })
  const blocked =
    (restricted && !enforceable) ||
    firstProblem !== null ||
    drafts.length > MAX_EGRESS_RULES
  const canSave = !disabled && !save.isPending && !blocked && dirty

  const setDraft = (index: number, patch: Partial<EgressRuleDraft>) =>
    setDrafts((rows) =>
      rows.map((row, i) => (i === index ? { ...row, ...patch } : row))
    )

  return (
    <Card>
      <CardHeader className="pb-3">
        <CardTitle className="flex items-center gap-2 text-sm font-medium">
          <Network className="h-4 w-4" />
          Outbound network
        </CardTitle>
      </CardHeader>
      <CardContent className="space-y-4">
        <div
          role="radiogroup"
          aria-label="Where this sandbox can connect"
          className="grid grid-cols-1 gap-2 md:grid-cols-2"
        >
          {MODES.map(({ value, title, description, icon: Icon }) => (
            <button
              key={value}
              type="button"
              role="radio"
              aria-checked={selected === value}
              disabled={disabled}
              onClick={() => setSelected(value)}
              className={cn(
                'flex flex-col gap-1 rounded-lg border p-3 text-left text-sm transition-colors',
                'disabled:cursor-not-allowed disabled:opacity-60',
                selected === value
                  ? 'border-primary bg-primary/5'
                  : 'hover:bg-muted/50'
              )}
            >
              <span className="flex items-center gap-2 font-medium">
                <Icon className="h-4 w-4" />
                {title}
              </span>
              <span className="text-xs text-muted-foreground">
                {description}
              </span>
            </button>
          ))}
        </div>

        {restricted && !enforceable && (
          <p role="alert" className="text-xs text-destructive">
            This sandbox runs on the {backend} backend, which cannot limit
            outbound traffic to a list. Create the sandbox on the Docker backend
            to use this.
          </p>
        )}

        {restricted && (
          <div className="space-y-2">
            <p className="text-xs text-muted-foreground">
              A hostname matches exactly;{' '}
              <span className="font-mono">*.example.com</span> matches its
              subdomains only. Leave ports empty for 80 and 443.
            </p>
            {drafts.length === 0 && (
              <p className="rounded-md border border-dashed p-3 text-xs text-muted-foreground">
                No destinations yet: the sandbox can reach nothing but Temps
                itself. Add a rule, e.g. registry.npmjs.org.
              </p>
            )}
            <ul className="space-y-2">
              {drafts.map((draft, index) => (
                <li key={index} className="space-y-1">
                  <div className="flex flex-col gap-2 sm:flex-row">
                    <select
                      aria-label={`Rule ${index + 1} type`}
                      value={draft.kind}
                      disabled={disabled}
                      onChange={(event) =>
                        setDraft(index, {
                          kind: event.target.value as EgressRuleDraft['kind'],
                        })
                      }
                      className="h-9 rounded-md border bg-background px-2 text-sm sm:w-[110px]"
                    >
                      <option value="host">Host</option>
                      <option value="cidr">IP / CIDR</option>
                    </select>
                    <Input
                      aria-label={`Rule ${index + 1} destination`}
                      value={draft.value}
                      disabled={disabled}
                      onChange={(event) =>
                        setDraft(index, { value: event.target.value })
                      }
                      placeholder={
                        draft.kind === 'host'
                          ? 'api.example.com'
                          : '198.51.100.0/24'
                      }
                      spellCheck={false}
                      className="h-9 min-w-0 flex-1 font-mono text-xs"
                      aria-invalid={problems[index] !== null}
                    />
                    <Input
                      aria-label={`Rule ${index + 1} ports`}
                      value={draft.ports}
                      disabled={disabled}
                      onChange={(event) =>
                        setDraft(index, { ports: event.target.value })
                      }
                      placeholder="80, 443"
                      spellCheck={false}
                      className="h-9 font-mono text-xs sm:w-[140px]"
                    />
                    <Button
                      type="button"
                      size="icon"
                      variant="ghost"
                      disabled={disabled}
                      onClick={() =>
                        setDrafts((rows) => rows.filter((_, i) => i !== index))
                      }
                      aria-label={`Remove rule ${index + 1}`}
                      className="h-9 w-9 shrink-0"
                    >
                      <Trash2 className="h-4 w-4" />
                    </Button>
                  </div>
                  {problems[index] && (
                    <p className="text-xs text-destructive">
                      {problems[index]}
                    </p>
                  )}
                </li>
              ))}
            </ul>
            <Button
              type="button"
              size="sm"
              variant="outline"
              disabled={disabled || drafts.length >= MAX_EGRESS_RULES}
              onClick={() =>
                setDrafts((rows) => [
                  ...rows,
                  { kind: 'host', value: '', ports: '' },
                ])
              }
            >
              <Plus className="mr-2 h-4 w-4" />
              Add destination
            </Button>
          </div>
        )}

        <p className="text-xs text-muted-foreground">
          Private, loopback and cloud-metadata addresses are never reachable,
          and Temps&rsquo; model and tool relays always work. Saving restarts
          the sandbox&rsquo;s network proxy, which drops open connections for a
          second or two.
        </p>

        <div className="flex justify-end">
          <Button
            type="button"
            size="sm"
            disabled={!canSave}
            onClick={() => save.mutate({ path: { id: sandboxId }, body: next })}
          >
            {save.isPending && (
              <Loader2 className="mr-2 h-4 w-4 animate-spin" />
            )}
            Save outbound network
          </Button>
        </div>
      </CardContent>
    </Card>
  )
}
