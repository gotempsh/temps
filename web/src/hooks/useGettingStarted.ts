import { useState } from 'react'
import { useActivationSignals } from './useActivationSignals'

const DISMISSED_KEY = 'temps_getting_started_dismissed'

export interface GettingStartedItem {
  key: string
  label: string
  description: string
  done: boolean
  href: string
  cta: string
}

export function useGettingStarted() {
  const [dismissed, setDismissed] = useState(
    () => localStorage.getItem(DISMISSED_KEY) === 'true'
  )

  const signals = useActivationSignals()

  const items: GettingStartedItem[] = [
    {
      key: 'git',
      label: 'Connect a Git provider',
      description: 'Link GitHub, GitLab, or Bitbucket to enable git-push deploys.',
      done: signals.gitConnected,
      // Navigate to the full page rather than a modal — the in-modal
      // GitProviderFlow overflows on smaller viewports (horizontal scroll).
      href: '/git-providers/add',
      cta: 'Connect Git',
    },
    // NOTE: "Deploy your first project" is intentionally omitted — this
    // checklist only renders once at least one project exists (the empty-
    // state onboarding owns the no-projects moment), so that step would
    // always be complete and add nothing.
    //
    // Ordered by importance: foundational platform setup that affects every
    // deployment (HTTPS routing, failure alerts, the DNS automation that backs
    // them) comes before per-app extras (databases and their backups), with
    // team collaboration last.
    {
      key: 'domain',
      label: 'Add a wildcard domain',
      description:
        'Point a wildcard DNS record at this server and get HTTPS for all apps.',
      done: signals.wildcardDomainReady,
      href: '/domains/add',
      cta: 'Add domain',
    },
    {
      key: 'notifications',
      label: 'Configure notifications',
      description: 'Get alerted on Slack, email, or webhook when deployments fail.',
      done: signals.notificationsConfigured,
      href: '/settings/notifications',
      cta: 'Set up',
    },
    {
      key: 'dns',
      label: 'Add a DNS provider',
      description:
        'Connect Cloudflare or Route53 for automatic wildcard SSL certificates.',
      done: signals.dnsProviderConnected,
      href: '/dns-providers/add',
      cta: 'Connect',
    },
    {
      key: 'database',
      label: 'Add a database',
      description:
        'Provision a managed Postgres, Redis, or MongoDB to attach to a project.',
      done: signals.hasDatabase,
      href: '/storage/create',
      cta: 'Add database',
    },
    {
      key: 'backups',
      label: 'Set up backups',
      description:
        'Schedule automatic backups so your databases are protected.',
      done: signals.backupsConfigured,
      href: '/backups',
      cta: 'Set up',
    },
    {
      key: 'team',
      label: 'Invite your team',
      description: 'Add teammates so they can manage projects and deployments.',
      done: signals.teamInvited,
      href: '/settings/users',
      cta: 'Invite',
    },
  ]

  const completedCount = items.filter((i) => i.done).length
  const totalCount = items.length
  const allDone = completedCount === totalCount
  // Only allow dismissing once at least one item is done — prevents new
  // users from instantly hiding the checklist before engaging with it.
  const canDismiss = completedCount >= 1
  const visible = signals.isLoaded && !allDone && !dismissed

  function dismiss() {
    localStorage.setItem(DISMISSED_KEY, 'true')
    setDismissed(true)
  }

  return {
    items,
    completedCount,
    totalCount,
    allDone,
    canDismiss,
    dismissed,
    dismiss,
    visible,
    isLoaded: signals.isLoaded,
  }
}
