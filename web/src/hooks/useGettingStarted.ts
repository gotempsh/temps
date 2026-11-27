// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import type { StepKey } from '@/components/welcome/onboarding'
import { useOnboarding } from '@/components/welcome/useOnboarding'
import { useActivationSignals } from './useActivationSignals'
import { useManualHarnessCompletion } from './useManualHarnessCompletion'

const DISMISSED_KEY = 'temps_getting_started_dismissed'

export interface GettingStartedItem {
  key: string
  label: string
  description: string
  done: boolean
  href: string
  cta: string
  /** Why the welcome flow's matching step was left for later, if it was. */
  laterReason?: string
}

/** The welcome flow's step each checklist item continues. */
const WELCOME_STEP: Partial<Record<string, StepKey>> = {
  deploy: 'app',
  ai: 'agents',
  domain: 'reach',
}

export function useGettingStarted() {
  const [dismissed, setDismissed] = useState(
    () => localStorage.getItem(DISMISSED_KEY) === 'true'
  )

  const signals = useActivationSignals()
  const manualHarness = useManualHarnessCompletion()
  const onboarding = useOnboarding()
  const laterReason = (key: string) => {
    const step = WELCOME_STEP[key]
    return step
      ? onboarding.data?.later?.find((l) => l.step === step)?.reason
      : undefined
  }

  const allItems: GettingStartedItem[] = [
    // Reuse project creation: template configuration and deployment progress
    // remain in their existing feature pages.
    {
      key: 'deploy',
      label: 'Deploy your first app',
      description:
        'Choose a starter template, configure your project, and follow its first deployment to a live URL.',
      done: signals.firstDeploySucceeded,
      href: '/projects/new?source=templates',
      cta: 'Deploy an app',
    },
    {
      key: 'ai',
      label: 'Connect your AI harness',
      description:
        'Create a dedicated admin key, install the Temps skill, and verify access from your harness.',
      done: signals.aiHarnessConfigured || manualHarness.completed,
      href: '/setup/ai',
      cta: 'Connect harness',
    },
    {
      key: 'git',
      label: 'Connect a Git provider',
      description:
        'Link GitHub, GitLab, or Bitbucket to enable git-push deploys.',
      done: signals.gitConnected,
      // Navigate to the full page rather than a modal — the in-modal
      // GitProviderFlow overflows on smaller viewports (horizontal scroll).
      href: '/git-providers/add',
      cta: 'Connect Git',
    },
    // Ordered by importance: foundational platform setup that affects every
    // deployment (HTTPS routing, failure alerts, the DNS automation that backs
    // them) comes before per-app extras (databases and their backups), with
    // team collaboration last.
    {
      key: 'domain',
      label: 'Make your apps reachable',
      description:
        'Give apps an address others can open: a wildcard preview domain pointed at this server, or a tunnel that publishes environment hosts.',
      done: signals.appsReachable,
      href: '/settings#preview-domain',
      cta: 'Set up',
    },
    {
      key: 'notifications',
      label: 'Configure notifications',
      description:
        'Get alerted on Slack, email, or webhook when deployments fail.',
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
        'Provision a managed Postgres, Redis, or MongoDB to attach to a service.',
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
      description: 'Add teammates so they can manage services and deployments.',
      done: signals.teamInvited,
      href: '/settings/users',
      cta: 'Invite',
    },
  ]

  // A step this role cannot read is left out rather than shown as "to do":
  // nothing was checked, so neither answer would be true.
  const items = allItems
    .filter(
      (item) =>
        !(item.key === 'dns' && signals.unreadable.dnsProviders) &&
        !(item.key === 'team' && signals.unreadable.team)
    )
    .map((item) => {
      const reason = item.done ? undefined : laterReason(item.key)
      return reason ? { ...item, laterReason: reason } : item
    })

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
    markHarnessCompleted: manualHarness.markCompleted,
    /** The welcome flow has not been finished or skipped. */
    welcomeOpen: onboarding.isSuccess && !onboarding.data.completed_at,
  }
}
