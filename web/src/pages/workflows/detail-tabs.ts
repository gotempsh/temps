// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getWorkflowSettingsOptions,
  listWorkflowGrantsOptions,
  listWorkflowProjectKeysOptions,
} from '@/api/client/@tanstack/react-query.gen'

export const workflowDetailTabs = [
  'runs',
  'settings',
  'access',
  'keys',
] as const
export type WorkflowDetailTab = (typeof workflowDetailTabs)[number]

/** The tab a `?tab=` value opens; anything unknown opens the runs. */
export function workflowDetailTab(value: string | null): WorkflowDetailTab {
  return workflowDetailTabs.find((tab) => tab === value) ?? 'runs'
}

export const workflowDetailTabLabels: Record<WorkflowDetailTab, string> = {
  runs: 'Runs',
  settings: 'Settings',
  access: 'Access',
  keys: 'Encryption keys',
}

export interface WorkflowCrumb {
  label: string
  href?: string
}

/**
 * Workflows › project › environment › workflow, plus the open tab. The header
 * never links the last crumb, so on any tab but Runs the workflow is followed
 * by the tab name and stays a link back to the workflow itself.
 */
export function workflowDetailCrumbs({
  projectId,
  environmentId,
  workflow,
  tab,
  project,
  environment,
}: {
  projectId: number
  environmentId: number
  workflow: string
  tab: WorkflowDetailTab
  project?: { name: string; slug: string }
  environment?: { name: string; slug: string }
}): WorkflowCrumb[] {
  const projectHref = project ? `/projects/${project.slug}` : '/projects'
  const crumbs: WorkflowCrumb[] = [
    { label: 'Workflows', href: '/workflows' },
    { label: project?.name ?? 'Project', href: projectHref },
    {
      label: environment?.name ?? 'Environment',
      href: project
        ? `${projectHref}/durable-workflows?${
            environment
              ? `env=${encodeURIComponent(environment.slug)}`
              : `environment=${environmentId}`
          }`
        : '/projects',
    },
    {
      label: workflow || 'Workflow',
      href: `/workflows/${projectId}/${environmentId}?workflow=${encodeURIComponent(workflow)}`,
    },
  ]
  if (tab !== 'runs') crumbs.push({ label: workflowDetailTabLabels[tab] })
  return crumbs
}

/** One page of the grants that let other environments call this one. */
export function workflowGrantsOptions(
  path: { project_id: number; environment_id: number },
  page: number
) {
  return listWorkflowGrantsOptions({ path, query: { page, page_size: 20 } })
}

/** One page of the project's workflow payload keys. */
export function workflowKeysOptions(project_id: number, page: number) {
  return listWorkflowProjectKeysOptions({
    path: { project_id },
    query: { page, page_size: 20 },
  })
}

/**
 * What Refresh reloads besides the runs and the overview: the list on the
 * open tab. Settings that loaded are left out, so a refresh never resets a
 * form being edited; settings that failed to load are retried, as their
 * error tells the user to refresh.
 */
export function openTabQueryKeys(
  tab: WorkflowDetailTab,
  path: { project_id: number; environment_id: number },
  page: number,
  settingsFailed = false
): readonly unknown[][] {
  switch (tab) {
    case 'access':
      return [workflowGrantsOptions(path, page).queryKey]
    case 'keys':
      return [workflowKeysOptions(path.project_id, page).queryKey]
    case 'settings':
      return settingsFailed
        ? [getWorkflowSettingsOptions({ path }).queryKey]
        : []
    case 'runs':
      return []
  }
}
