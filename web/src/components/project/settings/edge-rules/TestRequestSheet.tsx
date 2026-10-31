// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Separator } from '@/components/ui/separator'
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
} from '@/components/ui/sheet'
import { Textarea } from '@/components/ui/textarea'
import { cn } from '@/lib/utils'
import { Status, type StatusTone } from '@temps-sdk/ds'
import { useMemo, useState } from 'react'
import {
  evaluateRules,
  type Decision,
  type RuleOutcome,
  type SampleRequest,
} from './evaluate'
import { BOT_CLASSES, CONTINENTS } from './fields'
import type { EdgeRule } from './types'

export interface EnvironmentOption {
  id: number
  name: string
}

const ANY_ENVIRONMENT = 'any'
const NO_CONTINENT = 'unknown'

const DEFAULT_SAMPLE: SampleRequest = {
  method: 'GET',
  url: 'https://app.example.com/wp-login.php',
  ip: '198.51.100.20',
  country: 'US',
  continent: 'NA',
  asn: '',
  isHostingProvider: false,
  userAgent: 'Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0)',
  headers: '',
  ja4: '',
  botClass: 'human',
  environmentId: null,
  challengeCleared: false,
}

const OUTCOME_LABELS: Record<RuleOutcome, string> = {
  disabled: 'Disabled',
  other_environment: 'Other environment',
  not_reached: 'Not reached',
  no_match: 'No match',
  ambiguous_path: 'Skipped: ambiguous path',
  logged: 'Matched, logged',
  challenge_passed: 'Matched, challenge already passed',
  decided: 'Matched and decided',
}

function decisionSummary(
  decision: Decision,
  ruleName: (id: string) => string
): { tone: StatusTone; label: string; detail: string } {
  switch (decision.kind) {
    case 'forward':
      return {
        tone: 'ok',
        label: 'Forwarded',
        detail:
          'No rule decided this request. It continues to rate limiting, attack mode and password protection, then your app.',
      }
    case 'allow':
      return {
        tone: 'ok',
        label: 'Allowed',
        detail: `"${ruleName(decision.ruleId)}" allowed it. Later edge rules were skipped; rate limiting, attack mode and password protection still apply.`,
      }
    case 'block':
      return {
        tone: 'error',
        label: `Blocked (${decision.status})`,
        detail: `"${ruleName(decision.ruleId)}" returned ${decision.status}. The request never reached your app.`,
      }
    case 'challenge':
      return {
        tone: 'warn',
        label: 'Challenged',
        detail: `"${ruleName(decision.ruleId)}" served the challenge page. After passing it the visitor continues to the next rules.`,
      }
    case 'redirect':
      return {
        tone: 'idle',
        label: `Redirected (${decision.status})`,
        detail: `"${ruleName(decision.ruleId)}" redirected to ${decision.location}.`,
      }
  }
}

interface TestRequestSheetProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  rules: EdgeRule[]
  environments: EnvironmentOption[]
  /** Rule being edited; highlighted in the trace. */
  focusRuleId?: string
}

export function TestRequestSheet({
  open,
  onOpenChange,
  rules,
  environments,
  focusRuleId,
}: TestRequestSheetProps) {
  const [sample, setSample] = useState<SampleRequest>(DEFAULT_SAMPLE)
  const set = <K extends keyof SampleRequest>(
    key: K,
    value: SampleRequest[K]
  ) => setSample((s) => ({ ...s, [key]: value }))
  const result = useMemo(() => evaluateRules(rules, sample), [rules, sample])
  const names = useMemo(
    () => new Map(rules.map((r) => [r.id, r.name])),
    [rules]
  )
  const ruleName = (id: string) => names.get(id) || 'Untitled rule'
  const summary = decisionSummary(result.decision, ruleName)
  const { facts } = result

  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent className="w-full overflow-y-auto sm:max-w-xl">
        <SheetHeader>
          <SheetTitle>Test a request</SheetTitle>
          <SheetDescription>
            Describe a request and see which rule decides it. Nothing is sent;
            the result updates as you type.
          </SheetDescription>
        </SheetHeader>

        <div className="mt-6 space-y-6">
          <section aria-labelledby="test-result" className="space-y-3">
            <h3 id="test-result" className="text-sm font-semibold">
              Result
            </h3>
            <div className="space-y-2" aria-live="polite">
              <Status tone={summary.tone} label={summary.label} />
              <p className="text-sm text-muted-foreground">{summary.detail}</p>
              {facts.path !== facts.rawPath && (
                <p className="text-xs text-muted-foreground">
                  Rules see the path as{' '}
                  <code className="font-mono">{facts.path}</code> (received as{' '}
                  <code className="font-mono">{facts.rawPath}</code>).
                </p>
              )}
            </div>
            {rules.length === 0 ? (
              <p className="text-sm text-muted-foreground">
                There are no rules to evaluate yet.
              </p>
            ) : (
              <ol className="divide-y rounded-lg border">
                {result.trace.map((step, index) => (
                  <li
                    key={step.ruleId}
                    className={cn(
                      'flex items-center justify-between gap-3 px-3 py-2 text-sm',
                      step.ruleId === focusRuleId && 'bg-muted/60'
                    )}
                  >
                    <span className="min-w-0 truncate">
                      <span className="text-muted-foreground">
                        {index + 1}.
                      </span>{' '}
                      {ruleName(step.ruleId)}
                      {step.ruleId === focusRuleId && (
                        <span className="text-muted-foreground">
                          {' '}
                          (editing)
                        </span>
                      )}
                    </span>
                    <span
                      className={cn(
                        'shrink-0 text-xs',
                        step.outcome === 'decided'
                          ? 'font-medium text-foreground'
                          : 'text-muted-foreground'
                      )}
                    >
                      {OUTCOME_LABELS[step.outcome]}
                    </span>
                  </li>
                ))}
              </ol>
            )}
          </section>

          <Separator />

          <section aria-labelledby="test-request" className="space-y-4">
            <h3 id="test-request" className="text-sm font-semibold">
              Request
            </h3>
            <div className="grid grid-cols-[110px_1fr] gap-2">
              <div className="space-y-1.5">
                <Label htmlFor="test-method">Method</Label>
                <Select
                  value={sample.method}
                  onValueChange={(v) => set('method', v)}
                >
                  <SelectTrigger id="test-method">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {[
                      'GET',
                      'HEAD',
                      'POST',
                      'PUT',
                      'PATCH',
                      'DELETE',
                      'OPTIONS',
                    ].map((m) => (
                      <SelectItem key={m} value={m}>
                        {m}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
              <div className="min-w-0 space-y-1.5">
                <Label htmlFor="test-url">URL</Label>
                <Input
                  id="test-url"
                  value={sample.url}
                  className="font-mono"
                  onChange={(e) => set('url', e.target.value)}
                />
              </div>
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="test-headers">Headers</Label>
              <Textarea
                id="test-headers"
                rows={3}
                value={sample.headers}
                placeholder={'X-Api-Key: s3cret\nCookie: session=abc'}
                className="font-mono"
                onChange={(e) => set('headers', e.target.value)}
              />
              <p className="text-xs text-muted-foreground">
                One Name: value per line.
              </p>
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="test-ua">User agent</Label>
              <Input
                id="test-ua"
                value={sample.userAgent}
                onChange={(e) => set('userAgent', e.target.value)}
              />
            </div>
          </section>

          <section aria-labelledby="test-client" className="space-y-4">
            <h3 id="test-client" className="text-sm font-semibold">
              Client
            </h3>
            <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
              <div className="space-y-1.5">
                <Label htmlFor="test-ip">IP address</Label>
                <Input
                  id="test-ip"
                  value={sample.ip}
                  className="font-mono"
                  onChange={(e) => set('ip', e.target.value)}
                />
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="test-country">Country</Label>
                <Input
                  id="test-country"
                  value={sample.country}
                  maxLength={2}
                  placeholder="US"
                  onChange={(e) => set('country', e.target.value.toUpperCase())}
                />
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="test-continent">Continent</Label>
                <Select
                  value={sample.continent || NO_CONTINENT}
                  onValueChange={(v) =>
                    set('continent', v === NO_CONTINENT ? '' : v)
                  }
                >
                  <SelectTrigger id="test-continent">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value={NO_CONTINENT}>Unknown</SelectItem>
                    {CONTINENTS.map((c) => (
                      <SelectItem key={c.value} value={c.value}>
                        {c.label}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="test-asn">Network (ASN)</Label>
                <Input
                  id="test-asn"
                  value={sample.asn}
                  placeholder="64496"
                  onChange={(e) => set('asn', e.target.value)}
                />
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="test-bot">Bot class</Label>
                <Select
                  value={sample.botClass}
                  onValueChange={(v) => set('botClass', v)}
                >
                  <SelectTrigger id="test-bot">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {BOT_CLASSES.map((b) => (
                      <SelectItem key={b.value} value={b.value}>
                        {b.label}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="test-ja4">TLS fingerprint (JA4)</Label>
                <Input
                  id="test-ja4"
                  value={sample.ja4}
                  className="font-mono"
                  onChange={(e) => set('ja4', e.target.value)}
                />
              </div>
            </div>
            <div className="flex items-center gap-2">
              <Checkbox
                id="test-hosting"
                checked={sample.isHostingProvider}
                onCheckedChange={(v) => set('isHostingProvider', v === true)}
              />
              <Label htmlFor="test-hosting" className="font-normal">
                Comes from a hosting provider network
              </Label>
            </div>
          </section>

          <section aria-labelledby="test-context" className="space-y-4 pb-6">
            <h3 id="test-context" className="text-sm font-semibold">
              Context
            </h3>
            {environments.length > 0 && (
              <div className="space-y-1.5">
                <Label htmlFor="test-env">Environment</Label>
                <Select
                  value={
                    sample.environmentId === null
                      ? ANY_ENVIRONMENT
                      : String(sample.environmentId)
                  }
                  onValueChange={(v) =>
                    set(
                      'environmentId',
                      v === ANY_ENVIRONMENT ? null : Number(v)
                    )
                  }
                >
                  <SelectTrigger id="test-env">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value={ANY_ENVIRONMENT}>
                      Any environment
                    </SelectItem>
                    {environments.map((env) => (
                      <SelectItem key={env.id} value={String(env.id)}>
                        {env.name}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
            )}
            <div className="flex items-center gap-2">
              <Checkbox
                id="test-cleared"
                checked={sample.challengeCleared}
                onCheckedChange={(v) => set('challengeCleared', v === true)}
              />
              <Label htmlFor="test-cleared" className="font-normal">
                Visitor already passed a challenge
              </Label>
            </div>
          </section>
        </div>
      </SheetContent>
    </Sheet>
  )
}
