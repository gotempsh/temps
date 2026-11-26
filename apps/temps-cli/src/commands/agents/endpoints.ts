// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps agents` commands that map one to one onto an API operation, declared
 * as typed tables (see `src/lib/endpoint.ts`).
 */

import type { Command } from 'commander'
import {
  activateAiProvider,
  cancelRun,
  createAgent,
  createGlobalMcp,
  createMcp,
  deleteAgent,
  deleteGlobalMcp,
  deleteMcp,
  downloadGlobalSkillArchive,
  downloadSkillArchive,
  getAgent,
  getAiProviderHarnessUpdate,
  getCliStatus,
  getGlobalMcp,
  getGlobalSandboxStatus,
  getGlobalSkill,
  getMcp,
  getSandboxStatus,
  getSkill,
  importLocalAiProviderCredential,
  latestRunForSource,
  listAgentRuns,
  listAiProviders,
  listAllRuns,
  listGlobalMcps,
  listMcps,
  rebuildSandboxImage,
  refreshAiProviderModels,
  retryRun,
  revealGlobalMcpConfig,
  revealMcpConfig,
  runAiProviderPreflight,
  runAiProviderSmoke,
  saveAgentToken,
  saveAiProviderCredential,
  smokeTestAgent,
  streamEvents,
  updateAgent,
  updateAiProvider,
  updateAiProviderHarness,
  updateGlobalMcp,
  updateMcp,
  verifySavedAiProviderCredential,
} from '../../api/sdk.gen.js'
import { endpoint, topGroup } from '../../lib/endpoint.js'

export function registerAgentsEndpoints(program: Command): void {
  const group = topGroup(program, 'agents', "Project agents, their MCP servers, skills and AI providers")

  endpoint(group, createAgent, {
    name: "create",
    description: "Create agent",
    path: { project_id: 'project' },
    body: {
      ai_model: { kind: 'string', help: "Preferred model identifier for the CLI. 'Some(\"\")' clears the stored value" },
      ai_provider: 'string',
      ai_provider_key_id: 'int',
      api_key: { kind: 'secret', help: "Plain-text API key — will be encrypted before storage" },
      branch_prefix: 'string',
      config_repo_branch: { kind: 'string', help: "Branch of the config repo to use (default: \"main\")" },
      config_repo_url: { kind: 'string', help: "Private config repo containing .claude/ directory (skills, MCP, plugins)" },
      cooldown_minutes: 'int',
      daily_budget_cents: 'int',
      deliverable: 'string',
      description: 'string',
      enabled: 'bool',
      max_turns: 'int',
      mcp_servers_config: { kind: 'json', help: "MCP servers config (Claude Code settings.json mcpServers format)" },
      name: 'string',
      prompt: 'string',
      sandbox_enabled: 'bool',
      skills_config: { kind: 'json', help: "Skills config as JSON array" },
      slug: 'string',
      timeout_seconds: 'int',
      tools_config: { kind: 'json', help: "Tools config as JSON array" },
      trigger_config: { kind: 'json', help: "Trigger configuration JSON: { \"error\": { \"new_issue\": true, \"regression\": true }, \"manual\": true }" },
    },
  })

  endpoint(group, getCliStatus, {
    name: "get-cli-status",
    description: "Get cli status",
    path: { project_id: 'project' },
    query: {
      provider: { kind: 'string', help: "AI provider: claude_cli or codex_cli" },
    },
  })

  endpoint(group, listAllRuns, {
    name: "list-all-runs",
    description: "List all runs",
    path: { project_id: 'project' },
    query: {
      page: { kind: 'int', help: "Page number (1-based)" },
      page_size: { kind: 'int', help: "Page size (max 100)" },
    },
  })

  endpoint(group, latestRunForSource, {
    name: "latest-run-for-source",
    description: "Latest run for source",
    path: { project_id: 'project' },
    query: {
      trigger_source_type: { kind: 'string!', help: "Trigger source type, e.g. 'error_group'" },
      trigger_source_id: { kind: 'int!', help: "Trigger source ID" },
    },
  })

  endpoint(group, cancelRun, {
    name: "cancel-run",
    description: "Cancel run",
    path: { project_id: 'project', run_id: 'int' },
    confirm: "This will cancel run.",
  })

  endpoint(group, retryRun, {
    name: "retry-run",
    description: "Retry a completed, failed, cancelled, or no_fix run with the same trigger context",
    path: { project_id: 'project', run_id: 'int' },
  })

  endpoint(group, getSandboxStatus, {
    name: "get-sandbox-status",
    description: "Get sandbox status",
    path: { project_id: 'project' },
  })

  endpoint(group, smokeTestAgent, {
    name: "smoke-test",
    description: "Run a smoke test to verify the selected AI CLI works in the environment where agents will actually execute..",
    path: { project_id: 'project' },
    query: {
      provider_id: { kind: 'string', help: "Provider id to test; defaults to the globally active provider" },
    },
  })

  endpoint(group, deleteAgent, {
    name: "delete",
    description: "Delete agent",
    path: { project_id: 'project', slug: 'string' },
    confirm: "This will delete agent.",
  })

  endpoint(group, getAgent, {
    name: "get",
    description: "Get agent",
    path: { project_id: 'project', slug: 'string' },
  })

  endpoint(group, updateAgent, {
    name: "update",
    description: "Update agent",
    path: { project_id: 'project', slug: 'string' },
    body: {
      ai_model: { kind: 'string', help: "Preferred model identifier for the CLI. 'Some(\"\")' clears the stored value" },
      ai_provider: 'string',
      ai_provider_key_id: 'int',
      api_key: { kind: 'secret', help: "Plain-text API key — will be encrypted before storage" },
      branch_prefix: 'string',
      config_repo_branch: { kind: 'string', help: "Branch of the config repo to use (default: \"main\")" },
      config_repo_url: { kind: 'string', help: "Private config repo containing .claude/ directory (skills, MCP, plugins)" },
      cooldown_minutes: 'int',
      daily_budget_cents: 'int',
      deliverable: 'string',
      description: 'string',
      enabled: 'bool',
      max_turns: 'int',
      mcp_servers_config: { kind: 'json', help: "MCP servers config (Claude Code settings.json mcpServers format)" },
      name: 'string',
      prompt: 'string',
      sandbox_enabled: 'bool',
      skills_config: { kind: 'json', help: "Skills config as JSON array" },
      slug: 'string',
      timeout_seconds: 'int',
      tools_config: { kind: 'json', help: "Tools config as JSON array" },
      trigger_config: { kind: 'json', help: "Trigger configuration JSON: { \"error\": { \"new_issue\": true, \"regression\": true }, \"manual\": true }" },
    },
  })

  endpoint(group, listAgentRuns, {
    name: "list-runs",
    description: "List agent runs",
    path: { project_id: 'project', slug: 'string' },
    query: {
      page: { kind: 'int', help: "Page number (1-based)" },
      page_size: { kind: 'int', help: "Page size (max 100)" },
    },
  })

  endpoint(group, streamEvents, {
    name: "stream-events",
    description: "SSE endpoint: streams run log events in real-time",
    path: { project_id: 'project', run_id: 'int' },
    output: 'events',
  })

  endpoint(group, listMcps, {
    name: "list-mcps",
    description: "List mcps",
    path: { project_id: 'project' },
  })

  endpoint(group, createMcp, {
    name: "create-mcp",
    description: "Create mcp",
    path: { project_id: 'project' },
    body: {
      config: 'json!',
      description: 'string',
      name: 'string!',
      slug: 'string!',
    },
  })

  endpoint(group, deleteMcp, {
    name: "delete-mcp",
    description: "Delete mcp",
    path: { project_id: 'project', slug: 'string' },
    confirm: "This will delete mcp.",
  })

  endpoint(group, getMcp, {
    name: "get-mcp",
    description: "Get mcp",
    path: { project_id: 'project', slug: 'string' },
  })

  endpoint(group, updateMcp, {
    name: "update-mcp",
    description: "Update mcp",
    path: { project_id: 'project', slug: 'string' },
    body: {
      config: 'json!',
      description: 'string',
      name: 'string',
    },
  })

  endpoint(group, revealMcpConfig, {
    name: "reveal-mcp-config",
    description: "Reveal mcp config",
    path: { project_id: 'project', slug: 'string', field: 'string' },
  })

  endpoint(group, getSkill, {
    name: "get-skill",
    description: "Get skill",
    path: { project_id: 'project', slug: 'string' },
  })

  endpoint(group, downloadSkillArchive, {
    name: "download-skill-archive",
    description: "Download a skill's archive (tar.gz) — project-scoped",
    path: { project_id: 'project', slug: 'string' },
    output: 'bytes',
  })

  endpoint(group, saveAgentToken, {
    name: "save-token",
    description: "Save an encrypted AI provider token for use in sandbox containers",
    body: {
      token: { kind: 'secret!', help: "The OAuth token from 'claude setup-token' or an API key" },
    },
  })

  endpoint(group, listAiProviders, {
    name: "list-ai-providers",
    description: "List the AI provider catalog",
    query: {
      catalog_only: { kind: 'bool', help: "Return only static/cached catalog metadata" },
    },
  })

  endpoint(group, updateAiProvider, {
    name: "update-ai-provider",
    description: "Update provider-scoped settings without touching the saved credential",
    path: { provider_id: 'string' },
    body: {
      default_model: { kind: 'string', help: "New default model id. 'None' or an empty string clears the stored value so the CLI falls back to its own..." },
      max_turns_analysis: { kind: 'int', help: "Default max turns for the autofixer analysis phase (1–200). '0' clears the stored value (built-in default..." },
      max_turns_feedback: { kind: 'int', help: "Default max turns for autofixer feedback rounds (1–200). '0' clears; omitted leaves unchanged" },
      max_turns_fix: { kind: 'int', help: "Default max turns for the autofixer fix phase (1–200). '0' clears; omitted leaves unchanged" },
    },
  })

  endpoint(group, activateAiProvider, {
    name: "activate-ai-provider",
    description: "Activate a provider as the platform-wide default",
    path: { provider_id: 'string' },
  })

  endpoint(group, saveAiProviderCredential, {
    name: "save-ai-provider-credential",
    description: "Save (or replace) a provider's credential",
    path: { provider_id: 'string' },
    body: {
      auth_type: { kind: 'string!', help: "Auth flavor id (must match one of the provider's catalog entries)" },
      credential: { kind: 'secret!', help: "Plaintext credential body (API key, OAuth token, or full config file contents)" },
      verification_model: { kind: 'string', help: "OpenCode model to test, in 'provider/model' form" },
    },
  })

  endpoint(group, importLocalAiProviderCredential, {
    name: "import-local-ai-provider-credential",
    description: "Import the credential already used by this provider's CLI on the Temps host",
    path: { provider_id: 'string' },
    query: {
      verification_model: { kind: 'string', help: "OpenCode model to use for the verification request, in 'provider/model' form" },
    },
  })

  endpoint(group, verifySavedAiProviderCredential, {
    name: "verify-saved-ai-provider-credential",
    description: "Verify a previously imported OpenCode credential against a user-selected model without asking the browser..",
    path: { provider_id: 'string' },
    body: {
      verification_model: { kind: 'string!', help: "OpenCode model in 'provider/model' form, used to verify the saved secret" },
    },
  })

  endpoint(group, getAiProviderHarnessUpdate, {
    name: "get-ai-provider-harness-update",
    description: "Installed and latest version of the provider's harness CLI in your persistent workspace, and whether an..",
    path: { provider_id: 'string' },
    query: {
      refresh: { kind: 'bool', help: "Look inside the workspace again instead of returning the cached check" },
      cached_only: { kind: 'bool', help: "Return only what was last read, never looking inside the workspace (which could create or wake it)" },
    },
  })

  endpoint(group, updateAiProviderHarness, {
    name: "update-ai-provider-harness",
    description: "Install the latest release of the provider's harness CLI in your persistent workspace so newly released..",
    path: { provider_id: 'string' },
  })

  endpoint(group, refreshAiProviderModels, {
    name: "refresh-ai-provider-models",
    description: "Refresh exactly one provider's account-aware model inventory",
    path: { provider_id: 'string' },
  })

  endpoint(group, runAiProviderPreflight, {
    name: "run-ai-provider-preflight",
    description: "Run ai provider preflight",
    path: { provider_id: 'string' },
  })

  endpoint(group, runAiProviderSmoke, {
    name: "run-ai-provider-smoke",
    description: "Run ai provider smoke",
    path: { provider_id: 'string' },
    body: {
      consent: { kind: 'bool!', help: "Explicit acknowledgement that this check makes a billable model request" },
      model: { kind: 'string', help: "Optional catalog model identifier" },
    },
  })

  endpoint(group, listGlobalMcps, {
    name: "list-global-mcps",
    description: "List global mcps",
  })

  endpoint(group, createGlobalMcp, {
    name: "create-global-mcp",
    description: "Create global mcp",
    body: {
      config: 'json!',
      description: 'string',
      name: 'string!',
      slug: 'string!',
    },
  })

  endpoint(group, deleteGlobalMcp, {
    name: "delete-global-mcp",
    description: "Delete global mcp",
    path: { slug: 'string' },
    confirm: "This will delete global mcp.",
  })

  endpoint(group, getGlobalMcp, {
    name: "get-global-mcp",
    description: "Get global mcp",
    path: { slug: 'string' },
  })

  endpoint(group, updateGlobalMcp, {
    name: "update-global-mcp",
    description: "Update global mcp",
    path: { slug: 'string' },
    body: {
      config: 'json!',
      description: 'string',
      name: 'string',
    },
  })

  endpoint(group, revealGlobalMcpConfig, {
    name: "reveal-global-mcp-config",
    description: "Reveal global mcp config",
    path: { slug: 'string', field: 'string' },
  })

  endpoint(group, rebuildSandboxImage, {
    name: "rebuild-sandbox-image",
    description: "Rebuild sandbox image",
    output: 'events',
  })

  endpoint(group, getGlobalSandboxStatus, {
    name: "get-global-sandbox-status",
    description: "Get global sandbox status",
  })

  endpoint(group, getGlobalSkill, {
    name: "get-global-skill",
    description: "Get global skill",
    path: { slug: 'string' },
  })

  endpoint(group, downloadGlobalSkillArchive, {
    name: "download-global-skill-archive",
    description: "Download a skill's archive (tar.gz) — global",
    path: { slug: 'string' },
    output: 'bytes',
  })
}
