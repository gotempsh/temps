import { useQuery } from '@tanstack/react-query'
import {
  getSandboxStatusOptions,
  listAiProvidersOptions,
} from '@/api/client/@tanstack/react-query.gen'

/**
 * A single prerequisite for running AI autofix. The onboarding dialog renders
 * one row per step; `done` drives the check/CTA state and `to` links to the
 * exact settings page that resolves it.
 */
export interface AutofixReadinessStep {
  key: 'provider' | 'sandbox' | 'git'
  title: string
  /** Shown while the step is incomplete — what the user needs to do. */
  description: string
  /**
   * Compact noun phrase naming what's missing, for one-line surfaces that
   * can't fit `description` — reads as "Autofix needs {summary} …".
   */
  summary: string
  done: boolean
  /** Label for the resolve-this-step button. */
  ctaLabel: string
  /** Route the CTA navigates to. Git is project-scoped; the rest are global. */
  to: string
}

export interface AutofixReadiness {
  /** True once every applicable step is satisfied. */
  isReady: boolean
  /** Any underlying status query still loading — callers show a spinner. */
  isLoading: boolean
  steps: AutofixReadinessStep[]
  /** Convenience: the first unmet step, or null when ready. */
  firstIncomplete: AutofixReadinessStep | null
}

/**
 * Resolves whether AI autofix can run, as a list of onboarding steps.
 *
 * Three independent gates, each with its own settings surface:
 *  - **provider** — an AI provider credential is saved (`/settings/ai-providers`).
 *  - **sandbox** — Docker is reachable and the agent image is built.
 *  - **git** — the project has a git connection so the fix can open a PR.
 *
 * The provider + sandbox gates are platform-wide, so they resolve even when no
 * project is passed (e.g. a global "set up autofix" entry point). The git gate
 * only applies when a `projectGitConnected` value is supplied; global callers
 * omit it and the step is dropped from the list.
 */
export function useAutofixReadiness(opts?: {
  projectId?: number
  projectSlug?: string
  /**
   * The project's git state. Omit entirely for global callers (the git step is
   * then dropped from the list).
   *
   * A project can be linked to a **public** repository by URL alone, which
   * stores `repo_owner`/`repo_name` but leaves `git_provider_connection_id`
   * NULL (see `isPublicRepo` in GitSettings). That's read-only: autofix can't
   * push a branch or open a pull request with it, so it still counts as unmet
   * — but the wording has to say so rather than claim no repo is connected.
   */
  projectGit?: {
    /** `git_provider_connection_id != null` — authenticated, can push. */
    connected: boolean
    /** A repo is linked at all, even if only by public URL. */
    hasRepo: boolean
    /** `owner/name`, used to name the repo in the read-only message. */
    label?: string
  }
  /** Skip the network calls entirely (e.g. dialog closed). */
  enabled?: boolean
}): AutofixReadiness {
  const enabled = opts?.enabled ?? true

  const { data: catalog, isLoading: catalogLoading } = useQuery({
    ...listAiProvidersOptions(),
    enabled,
  })

  const { data: sandboxStatus, isLoading: sandboxLoading } = useQuery({
    ...getSandboxStatusOptions({
      path: { project_id: opts?.projectId ?? 0 },
    }),
    enabled: enabled && opts?.projectId !== undefined,
  })

  const providerConfigured = !!catalog?.providers?.some(
    (p) => p.credential_saved
  )
  const sandboxReady =
    !!sandboxStatus?.docker_available && !!sandboxStatus?.image_ready

  const steps: AutofixReadinessStep[] = [
    {
      key: 'provider',
      title: 'Connect an AI provider',
      description:
        'Save a credential for Claude or another provider so the agent can run.',
      summary: 'an AI provider credential',
      done: providerConfigured,
      ctaLabel: 'Configure provider',
      to: '/agent-sandbox/providers',
    },
    {
      key: 'sandbox',
      title: 'Prepare the sandbox',
      description:
        'Autofix runs inside an isolated container. Docker must be available and the agent image built.',
      summary: 'a working sandbox',
      done: sandboxReady,
      ctaLabel: 'Open sandbox settings',
      to: '/agent-sandbox/sandbox',
    },
  ]

  // Git only matters for a concrete project (that's where the PR is opened).
  if (opts?.projectGit) {
    const { connected, hasRepo, label } = opts.projectGit
    // A public URL-only link reads fine but can't push, so distinguish it from
    // "nothing connected" — otherwise the dialog tells you to connect a repo
    // while the settings page plainly shows one connected with a Public badge.
    const readOnly = !connected && hasRepo
    steps.push({
      key: 'git',
      title: readOnly
        ? 'Authorize write access to the repository'
        : 'Connect the repository',
      description: readOnly
        ? `${label ?? 'This repository'} is linked by public URL, which is read-only. Autofix pushes a branch and opens a pull request, so it needs a git provider connection with write access.`
        : 'Link a git provider with write access so autofix can open a pull request.',
      summary: readOnly
        ? 'write access to the repository'
        : 'a connected repository',
      done: connected,
      ctaLabel: readOnly ? 'Authorize access' : 'Connect repository',
      // Drops the user straight into the repo picker rather than the git
      // settings overview, so connecting is one click from onboarding. The
      // same page is where a public URL is swapped for a provider connection.
      to: opts.projectSlug
        ? `/projects/${opts.projectSlug}/git/change-repository`
        : '/settings/git-providers',
    })
  }

  const isLoading =
    enabled &&
    (catalogLoading || (opts?.projectId !== undefined && sandboxLoading))

  const firstIncomplete = steps.find((s) => !s.done) ?? null

  return {
    isReady: !isLoading && steps.every((s) => s.done),
    isLoading,
    steps,
    firstIncomplete,
  }
}
