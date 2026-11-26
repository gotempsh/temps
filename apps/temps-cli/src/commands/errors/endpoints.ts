// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps errors` commands that map one to one onto an API operation, declared
 * as typed tables (see `src/lib/endpoint.ts`).
 */

import type { Command } from 'commander'
import {
  addContext,
  closeAutofixIncident,
  createAlertRule,
  createPr,
  deleteAlertRule,
  deleteProjectFeedback,
  downloadAutofixPatch,
  getAlertRule,
  getAutofixCapability,
  getAutofixIncident,
  getProjectFeedback,
  getRun,
  hasErrorGroups,
  listAlertRules,
  listAutofixIncidentEvents,
  listAutofixIncidents,
  listClientOutcomes,
  listGlobalErrorGroups,
  listProjectFeedback,
  listTelemetryReleases,
  openAutofixIncident,
  reAnalyze,
  searchErrorGroupAssignees,
  setAutofixPaused,
  startAnalysis,
  startFix,
  stopAutofixIncident,
  updateAlertRule,
  updateAutofixSettings,
  updateErrorSavedSearch,
  updateProjectFeedback,
} from '../../api/sdk.gen.js'
import { endpoint, topGroup } from '../../lib/endpoint.js'

export function registerErrorsEndpoints(program: Command): void {
  const group = topGroup(program, 'errors', "errors")

  endpoint(group, listGlobalErrorGroups, {
    name: "list-global-groups",
    description: "List issues across every project visible to the caller",
    query: {
      page: 'int',
      page_size: 'int',
      project_id: 'int',
      status: 'string',
      search: 'string',
      start_date: 'string',
      end_date: 'string',
    },
  })

  endpoint(group, getAutofixCapability, {
    name: "autofix get-capability",
    description: "Whether autofix can run for the project, what a fix would become, and the project's autofix policy",
    path: { project_id: 'project' },
  })

  endpoint(group, listAutofixIncidents, {
    name: "autofix list-incidents",
    description: "The project's autofix incidents, newest first",
    path: { project_id: 'project' },
    query: {
      page: { kind: 'int', help: "Page number, from 1" },
      page_size: { kind: 'int', help: "Items per page" },
      status: { kind: 'string', help: "'open', 'closed', or one exact status" },
      error_group_id: { kind: 'int', help: "Only incidents of this error group" },
    },
  })

  endpoint(group, openAutofixIncident, {
    name: "autofix open-incident",
    description: "Ask autofix to reproduce and fix an error group",
    path: { project_id: 'project' },
    body: {
      error_group_id: { kind: 'int!', help: "The error group to fix" },
    },
  })

  endpoint(group, getAutofixIncident, {
    name: "autofix get-incident",
    description: "One autofix incident",
    path: { project_id: 'project', incident_id: 'string' },
  })

  endpoint(group, closeAutofixIncident, {
    name: "autofix close-incident",
    description: "Record the decision on a fix waiting for review",
    path: { project_id: 'project', incident_id: 'string' },
    body: {
      decision: { kind: 'string!', help: "'merged' (you merged the pull request), 'applied' (you applied the patch) or 'rejected'" },
    },
  })

  endpoint(group, listAutofixIncidentEvents, {
    name: "autofix list-incident-events",
    description: "The incident's timeline: every action the server, the agent and people took, oldest first",
    path: { project_id: 'project', incident_id: 'string' },
    query: {
      after_id: { kind: 'int', help: "Only events after this id; pass the last id seen to poll" },
      limit: { kind: 'int', help: "Default 100, max 500" },
    },
  })

  endpoint(group, downloadAutofixPatch, {
    name: "autofix download-patch",
    description: "The fix as a unified diff, for 'git apply'",
    path: { project_id: 'project', incident_id: 'string' },
    output: 'bytes',
  })

  endpoint(group, stopAutofixIncident, {
    name: "autofix stop-incident",
    description: "Stop an incident: cancels the agent's turn, deletes the sandbox and keeps the transcript and timeline",
    path: { project_id: 'project', incident_id: 'string' },
  })

  endpoint(group, setAutofixPaused, {
    name: "autofix set-paused",
    description: "Pause or resume autofix for the project",
    path: { project_id: 'project' },
    body: {
      paused: 'bool!',
    },
  })

  endpoint(group, updateAutofixSettings, {
    name: "autofix update-settings",
    description: "Change the project's autofix policy",
    path: { project_id: 'project' },
    body: {
      ai_model: { kind: 'string', help: "Empty string clears it" },
      ai_provider: { kind: 'string', help: "Empty string clears it (back to the assistant's default)" },
      min_confidence: { kind: 'int', help: "Confidence level, 0-5, an 'auto_merge' fix must reach" },
      mode: { kind: 'string', help: "'off', 'propose' or 'auto_merge'" },
      protected_paths: 'strings',
      test_command: { kind: 'string', help: "Empty string clears it" },
    },
  })

  endpoint(group, startAnalysis, {
    name: "autofixer start-analysis",
    description: "Start an autofixer analysis run for the given error group",
    path: { project_id: 'project' },
    body: {
      branch: { kind: 'string', help: "Branch to clone instead of the project's main branch" },
      error_group_id: 'int!',
      max_turns: { kind: 'int', help: "Per-run turn cap applied to every phase (1–200)" },
      model: { kind: 'string', help: "Model id for the chosen provider. 'None' uses the provider's saved default model" },
      provider: { kind: 'string', help: "AI provider id (\"claude_cli\", \"codex_cli\", \"opencode\"). 'None' uses the platform default provider" },
      user_context: { kind: 'string', help: "Free-text notes for the model (extra context about the error, retry guidance, constraints)" },
    },
  })

  endpoint(group, getRun, {
    name: "autofixer get-run",
    description: "Get a single autofixer run with its logs",
    path: { project_id: 'project', run_id: 'int' },
  })

  endpoint(group, addContext, {
    name: "autofixer add-context",
    description: "Append a user message to the run's context field",
    path: { project_id: 'project', run_id: 'int' },
    body: {
      message: 'string!',
    },
  })

  endpoint(group, createPr, {
    name: "autofixer create-pr",
    description: "Push the fix branch and create a pull request",
    path: { project_id: 'project', run_id: 'int' },
  })

  endpoint(group, startFix, {
    name: "autofixer start-fix",
    description: "Transition from analysis to fix phase",
    path: { project_id: 'project', run_id: 'int' },
  })

  endpoint(group, reAnalyze, {
    name: "autofixer re-analyze",
    description: "Continue the conversation with user feedback",
    path: { project_id: 'project', run_id: 'int' },
  })

  endpoint(group, listClientOutcomes, {
    name: "ingestion list-client-outcomes",
    description: "List client outcomes",
    path: { project_id: 'project' },
    query: {
      days: { kind: 'int', help: "Retained observation window, 1–31 days; default 7" },
      page: 'int',
      page_size: 'int',
    },
  })

  endpoint(group, listAlertRules, {
    name: "alert-rules list",
    description: "List all alert rules for a project",
    path: { project_id: 'project' },
  })

  endpoint(group, createAlertRule, {
    name: "alert-rules create",
    description: "Create a new alert rule",
    path: { project_id: 'project' },
    body: {
      cooldown_minutes: { kind: 'int', help: "Minimum minutes between notifications for same rule+group" },
      enabled: 'bool',
      environment_filter: { kind: 'int', help: "Optional environment ID to filter alerts" },
      error_level_filter: { kind: 'string', help: "Optional error type/level filter" },
      name: 'string!',
      notification_priority: { kind: 'string', help: "Notification priority: Low, Normal, High, Critical" },
      trigger_config: { kind: 'json', help: "Trigger-specific configuration (e.g., {\"count\": 100, \"window_minutes\": 60} for frequency)" },
      trigger_type: { kind: 'string!', help: "Trigger type: new_issue, regression, frequency, new_user, user_count, status_change" },
    },
  })

  endpoint(group, deleteAlertRule, {
    name: "alert-rules delete",
    description: "Delete an alert rule",
    path: { project_id: 'project', rule_id: 'int' },
    confirm: "This will delete an alert rule.",
  })

  endpoint(group, getAlertRule, {
    name: "alert-rules get",
    description: "Get a specific alert rule",
    path: { project_id: 'project', rule_id: 'int' },
  })

  endpoint(group, updateAlertRule, {
    name: "alert-rules update",
    description: "Update an existing alert rule",
    path: { project_id: 'project', rule_id: 'int' },
    body: {
      cooldown_minutes: 'int',
      enabled: 'bool',
      environment_filter: 'int',
      error_level_filter: 'string',
      name: 'string',
      notification_priority: 'string',
      trigger_config: 'json',
      trigger_type: 'string',
    },
  })

  endpoint(group, searchErrorGroupAssignees, {
    name: "search-group-assignees",
    description: "Search users and teams to assign an error group to",
    path: { project_id: 'project' },
    query: {
      q: { kind: 'string', help: "Text in the user's name or email, or the team's name" },
    },
  })

  endpoint(group, updateErrorSavedSearch, {
    name: "update-saved-search",
    description: "Rename, share or change the filters of your saved search",
    path: { project_id: 'project', saved_search_id: 'int' },
    body: {
      filters: { kind: 'json', help: "The list filters a saved search applies" },
      name: 'string',
      shared: 'bool',
    },
  })

  endpoint(group, listProjectFeedback, {
    name: "feedback list",
    description: "List project feedback",
    path: { project_id: 'project' },
    query: {
      status: 'string',
      page: 'int',
      page_size: 'int',
    },
  })

  endpoint(group, deleteProjectFeedback, {
    name: "feedback delete",
    description: "Delete project feedback",
    path: { project_id: 'project', feedback_id: 'int' },
    confirm: "This will delete project feedback.",
  })

  endpoint(group, getProjectFeedback, {
    name: "feedback get",
    description: "Get project feedback",
    path: { project_id: 'project', feedback_id: 'int' },
  })

  endpoint(group, updateProjectFeedback, {
    name: "feedback update",
    description: "Update project feedback",
    path: { project_id: 'project', feedback_id: 'int' },
    body: {
      status: 'string!',
    },
  })

  endpoint(group, hasErrorGroups, {
    name: "has-groups",
    description: "Check if project has any error groups",
    path: { project_id: 'project' },
  })

  endpoint(group, listTelemetryReleases, {
    name: "releases list-telemetry",
    description: "List telemetry releases",
    path: { project_id: 'project' },
    query: {
      release: { kind: 'string', help: "Exact SDK release identity; absent lists observed releases" },
      days: { kind: 'int', help: "Retained observation window, 1–31 days; default 7" },
      page: 'int',
      page_size: 'int',
    },
  })
}
