// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { createDnsProvider, putPrivateIngress } from '@/api/client'
import { requireDeliveryData } from '@/components/domains/delivery-errors'
import { Badge } from '@/components/ui/badge'
import { Input } from '@/components/ui/input'
import { ingressSetupUrl } from '@/components/nodes/ingress-setup'
import { useUpdateSettings } from '@/hooks/useSettings'
import { problemDetail } from '@/lib/api-problem'
import { Button, Callout, Field } from '@temps-sdk/ds'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useRef, useState } from 'react'
import { useNavigate } from 'react-router'
import {
  AUDIENCES,
  audienceStatus,
  accessPlan,
  domainField,
  domainProblem,
  flatNote,
  initialChoice,
  namingsFor,
  stageProblems,
  stagesFor,
  tokenPermissions,
  waysFor,
  type AccessChoice,
  type AccessStage,
  type BoxFacts,
} from './access'
import { ChoiceList } from './ChoiceList'
import { LATER_REASONS } from './onboarding'
import { saveReach, type ReachSaveProgress } from './reach-save'
import { useUpdateOnboarding } from './useOnboarding'

const STAGE_TITLE: Record<AccessStage, string> = {
  who: 'Who should reach your apps?',
  way: 'How does traffic get in?',
  naming: 'How are apps named?',
  domain: 'Which domain?',
  token: 'Paste a Cloudflare API token',
  confirm: 'Here is the plan',
}

/**
 * Step 2: who reaches the apps, the way in, the naming and the domain, one
 * question per screen, then a plan that says what each side does. Saving
 * records the default on the server; open ports also set the preview domain,
 * and a tunnel continues on the tunnel page, which owns its token.
 */
export function ReachStep({
  facts,
  onContinue,
}: {
  facts: BoxFacts
  onContinue: () => void
}) {
  const navigate = useNavigate()
  const [choice, setChoice] = useState<AccessChoice>(() => initialChoice(facts))
  const [stage, setStage] = useState<AccessStage>('who')
  const [touched, setTouched] = useState(false)
  const updateSettings = useUpdateSettings()
  const updateOnboarding = useUpdateOnboarding()
  const queryClient = useQueryClient()

  const stages = stagesFor(choice.way)
  const index = stages.indexOf(stage)
  const problems = stageProblems(choice, stage, facts)
  const set = (patch: Partial<AccessChoice>) =>
    setChoice((c) => ({ ...c, ...patch }))

  // What an earlier, failed Save already wrote, so trying again never
  // stores the pasted token in a second DNS provider.
  const saved = useRef<ReachSaveProgress>({})
  const save = useMutation({
    mutationFn: () =>
      saveReach(
        choice,
        facts,
        {
          setPreviewDomain: (domain) =>
            updateSettings.mutateAsync({ preview_domain: domain }),
          createDnsProvider: async (body) => {
            requireDeliveryData(await createDnsProvider({ body }))
            await queryClient.invalidateQueries({ queryKey: ['dnsProviders'] })
          },
          servePrivately: (listen) =>
            putPrivateIngress({
              body: { listen, tailscale_serve: false },
              throwOnError: true,
            }),
          recordReach: (reach) =>
            updateOnboarding.mutateAsync({
              default_reach: reach,
              done: ['reach'],
            }),
        },
        saved.current
      ),
    onSuccess: (reach) => {
      if (reach.way === 'tunnel') navigate(ingressSetupUrl({ via: 'tunnel' }))
      else onContinue()
    },
  })

  const later = useMutation({
    mutationFn: () =>
      updateOnboarding.mutateAsync({
        later: [{ step: 'reach', reason: LATER_REASONS.reach }],
      }),
    onSuccess: onContinue,
  })

  const next = () => {
    setTouched(true)
    if (problems.length) return
    setTouched(false)
    const following = stages[index + 1]
    if (following) setStage(following)
  }
  const back = () => {
    setTouched(false)
    const previous = stages[index - 1]
    if (previous) setStage(previous)
  }

  const ways = waysFor(choice.audience, facts)
  const plan = stage === 'confirm' ? accessPlan(choice, facts) : null
  const field = domainField(choice)
  const domainHint = domainProblem(choice.domain)

  return (
    <div className="space-y-6">
      <h2 className="text-lg font-semibold">{STAGE_TITLE[stage]}</h2>

      {stage === 'who' && (
        <ChoiceList
          label={STAGE_TITLE.who}
          columns={2}
          value={choice.audience}
          onChange={(audience) => {
            const first =
              waysFor(audience, facts).find(
                (w) => w.recommended && w.available
              ) ??
              waysFor(audience, facts).find((w) => w.available) ??
              waysFor(audience, facts)[0]
            set({
              audience,
              way: first.way,
              naming: first.way === 'tailnet_name' ? 'per_app' : choice.naming,
            })
          }}
          choices={AUDIENCES.map((a) => {
            const status = audienceStatus(a.id, facts)
            return {
              id: a.id,
              label: a.label,
              detail: a.detail,
              note: status.note,
              disabled: !status.available,
            }
          })}
        />
      )}

      {stage === 'way' && (
        <ChoiceList
          label={STAGE_TITLE.way}
          value={choice.way}
          onChange={(way) =>
            set({
              way,
              naming: way === 'tailnet_name' ? 'per_app' : choice.naming,
            })
          }
          choices={ways.map((w) => ({
            id: w.way,
            label: w.label,
            detail: w.detail,
            note: w.note,
            disabled: !w.available,
            badge:
              w.recommended && w.available ? (
                <Badge variant="secondary">Recommended</Badge>
              ) : undefined,
          }))}
        />
      )}

      {stage === 'naming' && (
        <ChoiceList
          label={STAGE_TITLE.naming}
          columns={2}
          value={choice.naming}
          onChange={(naming) => set({ naming })}
          choices={namingsFor(choice.way).map((n) => ({
            id: n.id,
            label: n.label,
            detail: n.detail,
            note: n.example,
          }))}
        />
      )}

      {stage === 'domain' && (
        <div className="space-y-3">
          <Field label={field.label} error={touched ? problems[0] : undefined}>
            {(props) => (
              <Input
                {...props}
                autoFocus
                value={choice.domain}
                placeholder={field.placeholder}
                onChange={(e) => set({ domain: e.target.value })}
                onKeyDown={(e) => e.key === 'Enter' && next()}
              />
            )}
          </Field>
          {domainHint?.suggestion ? (
            <Button
              variant="link"
              className="h-auto p-0"
              onClick={() => set({ domain: domainHint.suggestion ?? '' })}
            >
              Use {domainHint.suggestion}
            </Button>
          ) : null}
          {flatNote(choice) ? (
            <Callout tone="info">{flatNote(choice)}</Callout>
          ) : null}
        </div>
      )}

      {stage === 'token' && (
        <div className="space-y-3">
          <Field
            label="Cloudflare API token"
            error={touched ? problems[0] : undefined}
          >
            {(props) => (
              <Input
                {...props}
                type="password"
                autoComplete="off"
                value={choice.token}
                onChange={(e) => set({ token: e.target.value })}
              />
            )}
          </Field>
          <PermissionList way={choice.way} />
        </div>
      )}

      {plan && (
        <div className="space-y-4">
          <dl className="grid grid-cols-1 gap-3 sm:grid-cols-2">
            <div className="rounded-lg border p-3">
              <dt className="text-xs text-muted-foreground">
                Who can open an app
              </dt>
              <dd className="mt-1 text-sm font-medium">{plan.who}</dd>
            </div>
            <div className="rounded-lg border p-3">
              <dt className="text-xs text-muted-foreground">
                A first app would be at
              </dt>
              <dd className="mt-1 text-sm font-medium [overflow-wrap:anywhere]">
                {plan.example}
              </dd>
            </div>
          </dl>
          {plan.records.length > 0 && (
            <div className="rounded-lg border">
              <div className="border-b px-3 py-2 text-xs text-muted-foreground">
                DNS
              </div>
              {plan.records.map((r) => (
                <div
                  key={r.name}
                  className="flex flex-wrap items-center gap-x-4 gap-y-1 px-3 py-2 font-mono text-xs"
                >
                  <span>{r.type}</span>
                  <span className="[overflow-wrap:anywhere]">{r.name}</span>
                  <span className="text-muted-foreground">→</span>
                  <span className="[overflow-wrap:anywhere]">{r.value}</span>
                  <span className="ml-auto font-sans text-muted-foreground">
                    {r.by === 'you' ? 'you add it' : 'Temps adds it'}
                  </span>
                </div>
              ))}
            </div>
          )}
          <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
            <PlanList title="You" items={plan.you} />
            <PlanList title="Temps" items={plan.temps} />
          </div>
          {choice.way === 'tunnel' && <PermissionList way="tunnel" />}
          {plan.caveats.length > 0 && (
            <ul className="list-disc space-y-1 pl-5 text-xs text-muted-foreground">
              {plan.caveats.map((c) => (
                <li key={c}>{c}</li>
              ))}
            </ul>
          )}
        </div>
      )}

      {touched &&
        stage !== 'domain' &&
        stage !== 'token' &&
        problems.length > 0 && <Callout tone="warning">{problems[0]}</Callout>}
      {save.isError && (
        <Callout tone="error" title="Not saved">
          {problemDetail(
            save.error,
            'The choice could not be saved. Try again.'
          )}
        </Callout>
      )}

      <div className="flex flex-col gap-2 sm:flex-row sm:items-center">
        {stage === 'confirm' ? (
          <Button
            busy={save.isPending}
            busyLabel="Saving…"
            disabled={later.isPending}
            onClick={() => save.mutate()}
          >
            {choice.way === 'tunnel'
              ? 'Save and set up the tunnel'
              : choice.way === 'direct' && choice.naming === 'wildcard'
                ? `Use ${choice.domain.trim().toLowerCase()}`
                : 'Save'}
          </Button>
        ) : (
          <Button onClick={next}>Continue</Button>
        )}
        {index > 0 && (
          <Button variant="outline" onClick={back}>
            Back
          </Button>
        )}
        {/* One write at a time: a deferral saved while Save is still
            enabling private ingress would leave the step, and the save
            would go on to serve privately and mark the step done. */}
        <Button
          variant="ghost"
          busy={later.isPending}
          busyLabel="Saving…"
          disabled={save.isPending}
          onClick={() => later.mutate()}
        >
          Do this later
        </Button>
      </div>
    </div>
  )
}

function PlanList({ title, items }: { title: string; items: string[] }) {
  return (
    <div className="rounded-lg border p-3">
      <h3 className="text-xs text-muted-foreground">{title}</h3>
      <ol className="mt-2 list-decimal space-y-1 pl-5 text-sm">
        {items.map((item) => (
          <li key={item}>{item}</li>
        ))}
      </ol>
    </div>
  )
}

function PermissionList({ way }: { way: AccessChoice['way'] }) {
  return (
    <div className="rounded-lg border p-3 text-sm">
      <h3 className="text-xs text-muted-foreground">Token permissions</h3>
      <ul className="mt-2 space-y-1 font-mono text-xs">
        {tokenPermissions(way).map((p) => (
          <li key={p}>{p}</li>
        ))}
      </ul>
    </div>
  )
}
