import { useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { CheckCircle2, Circle, X, ChevronRight } from 'lucide-react'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Progress } from '@/components/ui/progress'
import { cn } from '@/lib/utils'
import { useActivationSignals } from '@/hooks/useActivationSignals'

const DISMISSED_KEY = 'temps_getting_started_dismissed'

interface ChecklistItem {
  label: string
  description: string
  done: boolean
  href: string
  cta: string
}

export function GettingStartedCard() {
  const navigate = useNavigate()
  const [dismissed, setDismissed] = useState(
    () => localStorage.getItem(DISMISSED_KEY) === 'true'
  )

  const signals = useActivationSignals()

  const items: ChecklistItem[] = [
    {
      label: 'Connect a Git provider',
      description: 'Link GitHub, GitLab, or Bitbucket to enable git-push deploys.',
      done: signals.gitConnected,
      // Navigate to the full page rather than a modal — the in-modal
      // GitProviderFlow overflows on smaller viewports (horizontal scroll).
      href: '/git-providers/add',
      cta: 'Connect Git',
    },
    {
      label: 'Deploy your first project',
      description: 'Connect a Git repo or push a Docker image to get a live URL.',
      done: signals.hasProject,
      href: '/projects/new',
      cta: 'Create project',
    },
    {
      label: 'Add a wildcard domain',
      description:
        'Point a wildcard DNS record at this server and get HTTPS for all apps.',
      done: signals.wildcardDomainReady,
      href: '/domains/add',
      cta: 'Add domain',
    },
    {
      label: 'Configure notifications',
      description: 'Get alerted on Slack, email, or webhook when deployments fail.',
      done: signals.notificationsConfigured,
      href: '/settings/notifications',
      cta: 'Set up',
    },
  ]

  const completedCount = items.filter((i) => i.done).length
  const allDone = completedCount === items.length
  const pct = Math.round((completedCount / items.length) * 100)
  // Only show dismiss button once at least one item is done — prevents new
  // users from instantly hiding the checklist before engaging with it.
  const canDismiss = completedCount >= 1

  // Auto-hide when all done and data is loaded
  if ((allDone && signals.isLoaded) || dismissed) return null
  if (!signals.isLoaded) return null

  function handleItemClick(item: ChecklistItem) {
    if (item.done) return
    navigate(item.href)
  }

  return (
      <Card className="border-border/60">
        <CardHeader className="pb-3">
          <div className="flex items-start justify-between gap-2">
            <div className="space-y-1">
              <CardTitle className="text-base font-semibold">
                Getting started
              </CardTitle>
              <p className="text-sm text-muted-foreground">
                {completedCount} of {items.length} done
              </p>
            </div>
            {canDismiss && (
              <Button
                variant="ghost"
                size="icon"
                className="h-7 w-7 shrink-0 text-muted-foreground"
                onClick={() => {
                  localStorage.setItem(DISMISSED_KEY, 'true')
                  setDismissed(true)
                }}
                aria-label="Dismiss getting started checklist"
              >
                <X className="h-4 w-4" />
              </Button>
            )}
          </div>
          {/* transition-all so the bar animates when a step completes */}
          <Progress
            value={pct}
            className="h-1.5 mt-1 [&>div]:transition-all [&>div]:duration-500"
          />
        </CardHeader>

        <CardContent className="pt-0 space-y-1">
          {items.map((item) => (
            <div
              key={item.label}
              role={item.done ? undefined : 'button'}
              tabIndex={item.done ? undefined : 0}
              onClick={() => handleItemClick(item)}
              onKeyDown={(e) => {
                if (!item.done && (e.key === 'Enter' || e.key === ' ')) {
                  e.preventDefault()
                  handleItemClick(item)
                }
              }}
              className={cn(
                'flex items-center gap-3 rounded-md px-2 py-2 transition-colors',
                item.done
                  ? 'opacity-60 cursor-default'
                  : 'hover:bg-muted/50 cursor-pointer'
              )}
            >
              {item.done ? (
                <CheckCircle2 className="h-4 w-4 shrink-0 text-emerald-500" />
              ) : (
                <Circle className="h-4 w-4 shrink-0 text-muted-foreground/50" />
              )}

              <div className="flex-1 min-w-0">
                <p
                  className={cn(
                    'text-sm font-medium leading-none',
                    item.done && 'line-through text-muted-foreground'
                  )}
                >
                  {item.label}
                </p>
                {!item.done && (
                  <p className="text-xs text-muted-foreground mt-0.5 truncate">
                    {item.description}
                  </p>
                )}
              </div>

              {!item.done && (
                <span className="shrink-0 text-xs text-muted-foreground flex items-center gap-0.5">
                  {item.cta}
                  <ChevronRight className="h-3 w-3" />
                </span>
              )}
            </div>
          ))}
        </CardContent>
      </Card>
  )
}
