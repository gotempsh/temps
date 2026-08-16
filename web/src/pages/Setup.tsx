import { useEffect } from 'react'
import { Link, useNavigate } from 'react-router'
import {
  ArrowRight,
  Bell,
  CheckCircle2,
  ChevronRight,
  Database,
  DatabaseBackup,
  GitBranch,
  Globe,
  Lightbulb,
  PartyPopper,
  ShieldCheck,
  Sparkles,
  Bot,
  Users,
  type LucideIcon,
} from 'lucide-react'
import { Card, CardContent } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { cn } from '@/lib/utils'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import {
  useGettingStarted,
  type GettingStartedItem,
} from '@/hooks/useGettingStarted'
import { useActivationSignals } from '@/hooks/useActivationSignals'

// Per-step icon so the checklist reads visually, not just as text rows.
const STEP_ICONS: Record<string, LucideIcon> = {
  ai: Bot,
  git: GitBranch,
  domain: Globe,
  notifications: Bell,
  dns: ShieldCheck,
  database: Database,
  backups: DatabaseBackup,
  team: Users,
}

interface Recommendation {
  key: string
  icon: LucideIcon
  title: string
  body: string
  href: string
  cta: string
  tone: 'warning' | 'default'
}

// Context-aware, best-practice nudges derived from real activation signals.
// Distinct from the fixed setup checklist: these only surface when the
// current state actually warrants them (e.g. databases with no backups).
function buildRecommendations(
  s: ReturnType<typeof useActivationSignals>
): Recommendation[] {
  const recs: Recommendation[] = []

  if (!s.aiProviderConfigured) {
    recs.push({
      key: 'ai-provider',
      icon: Sparkles,
      tone: 'default',
      title: 'Enable built-in AI',
      body: 'Separately connect a model provider for Temps chat, autofix, and AI summaries.',
      href: '/ai-gateway',
      cta: 'Choose provider',
    })
  }

  if (s.hasDatabase && !s.backupsConfigured) {
    recs.push({
      key: 'backups',
      icon: DatabaseBackup,
      tone: 'warning',
      title: 'Protect your databases',
      body: 'You have managed databases but no backup schedule — one failure could mean data loss.',
      href: '/backups',
      cta: 'Schedule backups',
    })
  }

  if (!s.externalUrlSet) {
    recs.push({
      key: 'external-url',
      icon: Globe,
      tone: 'default',
      title: 'Set your external URL',
      body: 'OAuth callbacks, webhooks, and integrations need a stable public URL to work correctly.',
      href: '/settings',
      cta: 'Configure',
    })
  }

  if (s.wildcardDomainReady && !s.dnsProviderConnected) {
    recs.push({
      key: 'dns-auto',
      icon: ShieldCheck,
      tone: 'default',
      title: 'Automate SSL renewals',
      body: 'Connect a DNS provider so wildcard certificates renew automatically via DNS-01.',
      href: '/dns-providers/add',
      cta: 'Connect DNS',
    })
  }

  if (!s.notificationsConfigured) {
    recs.push({
      key: 'alerts',
      icon: Bell,
      tone: 'default',
      title: 'Never miss a failed deploy',
      body: 'Wire up Slack, email, or a webhook so you hear about failures before your users do.',
      href: '/settings/notifications',
      cta: 'Set up alerts',
    })
  }

  return recs
}

export function Setup() {
  const navigate = useNavigate()
  const { setBreadcrumbs } = useBreadcrumbs()
  const { items, completedCount, totalCount, allDone, isLoaded } =
    useGettingStarted()
  const signals = useActivationSignals()

  useEffect(() => {
    setBreadcrumbs([{ label: 'Finish setting up' }])
  }, [setBreadcrumbs])

  usePageTitle('Finish setting up')

  const pct = Math.round((completedCount / totalCount) * 100)
  const nextStep = items.find((i) => !i.done)
  const recommendations = buildRecommendations(signals)

  function goToStep(item: GettingStartedItem) {
    if (item.done) return
    navigate(item.href)
  }

  return (
    <div className="p-4 sm:p-8 space-y-6">
      <div>
        <h1 className="text-2xl font-semibold tracking-tight">
          Finish setting up
        </h1>
        <p className="text-sm text-muted-foreground mt-1">
          A few one-time steps to get your platform production-ready.
        </p>
      </div>

      {!isLoaded ? (
        <div className="space-y-6">
          <Skeleton className="h-40 w-full rounded-xl" />
          <div className="grid gap-6 lg:grid-cols-3">
            <Skeleton className="h-96 rounded-xl lg:col-span-2" />
            <Skeleton className="h-96 rounded-xl" />
          </div>
        </div>
      ) : allDone ? (
        <Card>
          <CardContent className="flex flex-col items-center gap-3 py-20 text-center">
            <div className="flex size-14 items-center justify-center rounded-full bg-emerald-500/10">
              <PartyPopper className="h-7 w-7 text-emerald-500" />
            </div>
            <div>
              <p className="text-lg font-semibold">You&apos;re all set</p>
              <p className="text-sm text-muted-foreground mt-1 max-w-sm">
                Every setup step is complete. Your platform is production-ready.
              </p>
            </div>
            <Button asChild className="mt-2">
              <Link to="/projects">Go to projects</Link>
            </Button>
          </CardContent>
        </Card>
      ) : (
        <>
          {/* Next-step hero — the single clearest "do this next" action. */}
          {nextStep && (
            <NextStepHero
              step={nextStep}
              completedCount={completedCount}
              totalCount={totalCount}
              pct={pct}
              onContinue={() => goToStep(nextStep)}
            />
          )}

          <div className="grid gap-6 lg:grid-cols-3">
            {/* All steps */}
            <section className="lg:col-span-2 space-y-3">
              <h2 className="text-sm font-semibold text-muted-foreground uppercase tracking-wide">
                All setup steps
              </h2>
              <Card>
                <CardContent className="p-2">
                  <div className="divide-y divide-border/60">
                    {items.map((item) => {
                      const Icon = STEP_ICONS[item.key] ?? CheckCircle2
                      const isNext = nextStep?.key === item.key
                      return (
                        <div
                          key={item.key}
                          role={item.done ? undefined : 'button'}
                          tabIndex={item.done ? undefined : 0}
                          onClick={() => goToStep(item)}
                          onKeyDown={(e) => {
                            if (
                              !item.done &&
                              (e.key === 'Enter' || e.key === ' ')
                            ) {
                              e.preventDefault()
                              goToStep(item)
                            }
                          }}
                          className={cn(
                            'flex items-center gap-4 rounded-lg px-3 py-3.5 transition-colors',
                            item.done
                              ? 'opacity-60 cursor-default'
                              : 'hover:bg-muted/50 cursor-pointer',
                            isNext && 'bg-primary/5'
                          )}
                        >
                          <div
                            className={cn(
                              'flex size-9 shrink-0 items-center justify-center rounded-lg border',
                              item.done
                                ? 'border-emerald-500/30 bg-emerald-500/10 text-emerald-500'
                                : 'bg-muted text-muted-foreground'
                            )}
                          >
                            {item.done ? (
                              <CheckCircle2 className="h-4 w-4" />
                            ) : (
                              <Icon className="h-4 w-4" />
                            )}
                          </div>

                          <div className="flex-1 min-w-0">
                            <div className="flex items-center gap-2">
                              <p
                                className={cn(
                                  'text-sm font-medium',
                                  item.done &&
                                    'line-through text-muted-foreground'
                                )}
                              >
                                {item.label}
                              </p>
                              {isNext && (
                                <span className="rounded-full bg-primary/10 px-2 py-0.5 text-[10px] font-medium text-primary">
                                  Up next
                                </span>
                              )}
                            </div>
                            {!item.done && (
                              <p className="text-xs text-muted-foreground mt-0.5">
                                {item.description}
                              </p>
                            )}
                          </div>

                          {item.done ? (
                            <span className="shrink-0 text-xs text-emerald-500">
                              Done
                            </span>
                          ) : (
                            <span className="shrink-0 text-xs text-muted-foreground flex items-center gap-0.5">
                              {item.cta}
                              <ChevronRight className="h-3.5 w-3.5" />
                            </span>
                          )}
                        </div>
                      )
                    })}
                  </div>
                </CardContent>
              </Card>
            </section>

            {/* Recommendations rail */}
            <aside className="space-y-3">
              <h2 className="text-sm font-semibold text-muted-foreground uppercase tracking-wide flex items-center gap-1.5">
                <Sparkles className="h-3.5 w-3.5" />
                Recommendations
              </h2>
              {recommendations.length > 0 ? (
                <div className="space-y-3">
                  {recommendations.map((rec) => (
                    <Link
                      key={rec.key}
                      to={rec.href}
                      className="group block rounded-xl border bg-card p-4 transition-colors hover:border-primary/40 hover:bg-accent/40"
                    >
                      <div className="flex items-start gap-3">
                        <div
                          className={cn(
                            'flex size-8 shrink-0 items-center justify-center rounded-lg',
                            rec.tone === 'warning'
                              ? 'bg-amber-500/10 text-amber-500'
                              : 'bg-primary/10 text-primary'
                          )}
                        >
                          <rec.icon className="h-4 w-4" />
                        </div>
                        <div className="min-w-0">
                          <p className="text-sm font-medium">{rec.title}</p>
                          <p className="text-xs text-muted-foreground mt-1 leading-relaxed">
                            {rec.body}
                          </p>
                          <span className="mt-2 inline-flex items-center gap-0.5 text-xs font-medium text-primary">
                            {rec.cta}
                            <ArrowRight className="h-3 w-3 transition-transform group-hover:translate-x-0.5" />
                          </span>
                        </div>
                      </div>
                    </Link>
                  ))}
                </div>
              ) : (
                <Card className="bg-muted/40">
                  <CardContent className="flex items-start gap-3 p-4">
                    <Lightbulb className="h-4 w-4 shrink-0 text-muted-foreground mt-0.5" />
                    <p className="text-xs text-muted-foreground leading-relaxed">
                      No recommendations right now — you&apos;re following the
                      best-practice defaults. New ones appear here as your setup
                      evolves.
                    </p>
                  </CardContent>
                </Card>
              )}
            </aside>
          </div>
        </>
      )}
    </div>
  )
}

function NextStepHero({
  step,
  completedCount,
  totalCount,
  pct,
  onContinue,
}: {
  step: GettingStartedItem
  completedCount: number
  totalCount: number
  pct: number
  onContinue: () => void
}) {
  const Icon = STEP_ICONS[step.key] ?? Sparkles
  return (
    <Card className="overflow-hidden border-primary/20">
      <CardContent className="p-0">
        <div
          className="p-5 sm:p-6"
          style={{
            backgroundImage:
              'radial-gradient(120% 120% at 0% 0%, color-mix(in oklch, var(--primary) 8%, transparent) 0%, transparent 55%)',
          }}
        >
          <div className="flex flex-col gap-5 sm:flex-row sm:items-center sm:justify-between">
            <div className="flex items-start gap-4 min-w-0">
              <div className="flex size-11 shrink-0 items-center justify-center rounded-xl bg-primary/10 text-primary">
                <Icon className="h-5 w-5" />
              </div>
              <div className="min-w-0">
                <p className="text-xs font-medium text-primary">
                  Continue setup — up next
                </p>
                <h2 className="text-lg font-semibold tracking-tight mt-0.5">
                  {step.label}
                </h2>
                <p className="text-sm text-muted-foreground mt-1 max-w-xl">
                  {step.description}
                </p>
              </div>
            </div>
            <Button size="lg" className="shrink-0" onClick={onContinue}>
              {step.cta}
              <ArrowRight className="h-4 w-4 ml-1.5" />
            </Button>
          </div>

          <div className="mt-5 flex items-center gap-3">
            <div className="h-1.5 flex-1 overflow-hidden rounded-full bg-muted">
              <div
                className="h-full rounded-full bg-primary transition-all duration-500"
                style={{ width: `${pct}%` }}
              />
            </div>
            <span className="text-xs font-medium tabular-nums text-muted-foreground">
              {completedCount} of {totalCount} done
            </span>
          </div>
        </div>
      </CardContent>
    </Card>
  )
}
