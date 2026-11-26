// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps ai` commands that map one to one onto an API operation, declared
 * as typed tables (see `src/lib/endpoint.ts`).
 */

import type { Command } from 'commander'
import {
  acceptBlueprintVersion,
  addProviderModel,
  appendAttachmentChunk,
  archiveApplication,
  archiveConversation,
  archiveUserConversation,
  attachApplicationWorkspaceTerminal,
  attachGlobalWorkspaceTerminal,
  bindApplicationGitConnection,
  bindBlueprintComponent,
  cancelAttachmentUpload,
  cancelBatch,
  cancelGoal,
  chatCompletions,
  compactConversation,
  completeAttachmentUpload,
  confirmPendingAction,
  confirmUserPendingAction,
  connectChatSandboxProject,
  connectChatSandboxService,
  controlApplicationWorkspace,
  createApplication,
  createApplicationConversation,
  createApplicationPreviewLink,
  createApplicationProject,
  createAssistantConversation,
  createBatch,
  createConversation,
  createGlobalConversation,
  createGlobalWorkspacePreviewLink,
  createProviderKey,
  createResponseJson,
  createResponseStream,
  createThreadArtifact,
  deleteFile,
  deleteProviderKey,
  deleteProviderModel,
  deployApplicationWorkspaceProject,
  discardSecret,
  disconnectApplicationGitConnection,
  disconnectChatSandboxProject,
  disconnectChatSandboxService,
  discoverProviderModels,
  downloadApplicationWorkspaceFile,
  downloadGlobalWorkspaceFile,
  embeddings,
  fileContent,
  findConversation,
  getAiProviderStatus,
  getApplication,
  getApplicationWorkspace,
  getApplicationWorkspaceChanges,
  getApplicationWorkspaceDiff,
  getApplicationWorkspaceDirectory,
  getApplicationWorkspaceFile,
  getBlueprint,
  getChatSandbox,
  getChatSandboxChanges,
  getChatSandboxDiff,
  getChatSuggestionMode,
  getChatWorkspace,
  getConversation,
  getConversationDetail,
  getConversations,
  getGlobalAiWorkspace,
  getGlobalWorkspaceChanges,
  getGlobalWorkspaceDiff,
  getGlobalWorkspaceDirectory,
  getGlobalWorkspaceFile,
  getLimitsOverview,
  getModelCatalog,
  getPendingAction,
  getPricing,
  getProjectLimits,
  getProviderKey,
  getSuggestionDefault,
  getUsageByProject,
  getUsageByProvider,
  getUsageRecent,
  getUsageSummary,
  getUsageTimeseries,
  getUsageTopModels,
  getUserConversation,
  getUserConversationAttachment,
  getUserConversationDiagnostics,
  getUserConversationUsage,
  getUserPendingAction,
  getWorkspaceActivity,
  getWorkspaceFileLimits,
  importApplicationWorkspaceGit,
  leaveChatWorkspace,
  linkApplicationProject,
  linkChatSandbox,
  listAllConversations,
  listApplicationConversations,
  listApplicationGitConnections,
  listApplications,
  listBlueprintVersions,
  listChatOperations,
  listChatResources,
  listChatSandboxFiles,
  listCodingAgents,
  listConversations,
  listGoals,
  listModelCatalogEntries,
  listModels,
  listPendingActions,
  listProviderKeys,
  listSecretRequests,
  listThreadArtifacts,
  listUnpricedModels,
  listUserPendingActions,
  provideSecret,
  readChatSandboxFile,
  refreshAiProviderStatus,
  refreshModelCatalog,
  refreshProviderModels,
  rejectBlueprintVersion,
  rejectPendingAction,
  rejectUserPendingAction,
  renameApplication,
  renameConversation,
  renameUserConversation,
  resetProjectLimits,
  resolvePermission,
  resolveUserPermission,
  restoreApplication,
  restoreUserConversation,
  retrieveBatch,
  retrieveFile,
  retryGoal,
  saveBlueprintVersion,
  sendProjectAiMessage,
  sendUserMessage,
  setApplicationPrimaryProject,
  setChatWorkspace,
  startAttachmentUpload,
  stopTurn,
  stopUserTurn,
  streamStructuredOutput,
  suggestNextSteps,
  testProviderKeyById,
  testProviderKeyInline,
  trackChatSandboxChanges,
  unbindBlueprintComponent,
  unlinkApplicationProject,
  unlinkChatSandbox,
  updateAiSummaryPreference,
  updateApplicationWorkspace,
  updateChatSuggestionMode,
  updateCodingAgentSettings,
  updateInstanceLimits,
  updateModelCatalogSettings,
  updatePermissionMode,
  updateProjectLimits,
  updateProviderKey,
  updateProviderModel,
  updateSuggestionDefault,
  updateUserPermissionMode,
  uploadApplicationWorkspaceFiles,
  uploadFile,
  uploadGlobalWorkspaceFiles,
  uploadUserConversationAttachment,
  wakeChatSandbox,
  writeApplicationWorkspaceFiles,
} from '../../api/sdk.gen.js'
import { endpoint, topGroup } from '../../lib/endpoint.js'

export function registerAiEndpoints(program: Command): void {
  const group = topGroup(program, 'ai', "ai")

  endpoint(group, listApplications, {
    name: "apps list-applications",
    description: "List applications",
    query: {
      status: { kind: 'string', help: "Resource lifecycle state (defaults to active)", values: ["active","archived"] },
      page: { kind: 'int', help: "Page number (1-indexed)" },
      page_size: { kind: 'int', help: "Number of applications or conversations per page (clamped to 1..=100)" },
    },
  })

  endpoint(group, createApplication, {
    name: "apps create-application",
    description: "Create application",
    body: {
      description: 'string',
      name: 'string!',
      project_ids: 'ints',
      starter_project: 'json',
    },
  })

  endpoint(group, archiveApplication, {
    name: "apps archive-application",
    description: "Archive an AI application",
    path: { application_public_id: 'string' },
    confirm: "This will archive an AI application.",
  })

  endpoint(group, getApplication, {
    name: "apps get-application",
    description: "Get application",
    path: { application_public_id: 'string' },
    query: {
      status: { kind: 'string', help: "Resource lifecycle state (defaults to active)", values: ["active","archived"] },
    },
  })

  endpoint(group, renameApplication, {
    name: "apps rename-application",
    description: "Rename an AI application",
    path: { application_public_id: 'string' },
    body: {
      name: { kind: 'string!', help: "New name shown for the application" },
    },
  })

  endpoint(group, listApplicationConversations, {
    name: "apps list-application-conversations",
    description: "List application conversations",
    path: { application_public_id: 'string' },
    query: {
      status: { kind: 'string', help: "Resource lifecycle state (defaults to active)", values: ["active","archived"] },
      page: { kind: 'int', help: "Page number (1-indexed)" },
      page_size: { kind: 'int', help: "Number of applications or conversations per page (clamped to 1..=100)" },
    },
  })

  endpoint(group, createApplicationConversation, {
    name: "apps create-application-conversation",
    description: "Create application conversation",
    path: { application_public_id: 'string' },
    body: {
      ai_provider: { kind: 'string', help: "A registered development harness (for example 'claude_cli')" },
      kind: { kind: 'string', help: "'ai' (default) or 'terminal'" },
    },
  })

  endpoint(group, listThreadArtifacts, {
    name: "apps list-thread-artifacts",
    description: "List thread artifacts",
    path: { application_public_id: 'string', conversation_public_id: 'string' },
  })

  endpoint(group, createThreadArtifact, {
    name: "apps create-thread-artifact",
    description: "Create thread artifact",
    path: { application_public_id: 'string', conversation_public_id: 'string' },
    body: {
      kind: 'string!',
      payload: 'json!',
      title: 'string',
    },
  })

  endpoint(group, listApplicationGitConnections, {
    name: "apps list-application-git-connections",
    description: "List application git connections",
    path: { application_public_id: 'string' },
  })

  endpoint(group, bindApplicationGitConnection, {
    name: "apps bind-application-git-connection",
    description: "Bind application git connection",
    path: { application_public_id: 'string' },
    body: {
      connectionId: 'int!',
      projectId: 'int!',
      remoteName: 'string!',
      repositoryId: 'int!',
    },
  })

  endpoint(group, disconnectApplicationGitConnection, {
    name: "apps disconnect-application-git-connection",
    description: "Disconnect application git connection",
    path: { application_public_id: 'string', binding_id: 'int' },
    confirm: "This will disconnect application git connection.",
  })

  endpoint(group, createApplicationPreviewLink, {
    name: "apps create-application-preview-link",
    description: "Mint a short-lived URL for a port running in the application harness sandbox",
    path: { application_public_id: 'string' },
    body: {
      path: { kind: 'string', help: "Optional same-origin path to open after the preview grant is exchanged" },
      port: { kind: 'int!', help: "The development server port detected or selected in the sandbox" },
    },
  })

  endpoint(group, createApplicationProject, {
    name: "apps create-application-project",
    description: "Create and link an application project",
    path: { application_public_id: 'string' },
    body: {
      exposed_port: 'int',
      name: 'string!',
      preset: { kind: 'string', help: "Deployment preset for the project" },
    },
  })

  endpoint(group, linkApplicationProject, {
    name: "apps link-application",
    description: "Link an existing project to an application",
    path: { application_public_id: 'string' },
    body: {
      project_id: 'int!',
    },
  })

  endpoint(group, unlinkApplicationProject, {
    name: "apps unlink-application",
    description: "Unlink an application project",
    path: { application_public_id: 'string', project_id: 'project' },
    confirm: "This will unlink an application project.",
  })

  endpoint(group, deployApplicationWorkspaceProject, {
    name: "apps deploy-application-workspace",
    description: "Deploy an application workspace project with Drop",
    path: { application_public_id: 'string', project_id: 'project' },
    body: {
      environment_id: { kind: 'int', help: "Target environment" },
    },
  })

  endpoint(group, setApplicationPrimaryProject, {
    name: "apps set-application-primary",
    description: "Choose the application's primary project",
    path: { application_public_id: 'string', project_id: 'project' },
  })

  endpoint(group, writeApplicationWorkspaceFiles, {
    name: "apps write-application-workspace-files",
    description: "Write a bounded batch of local files into an application project",
    path: { application_public_id: 'string', project_id: 'project' },
    body: {
      files: 'json!',
    },
  })

  endpoint(group, importApplicationWorkspaceGit, {
    name: "apps import-application-workspace-git",
    description: "Import a Git repository into an application project",
    path: { application_public_id: 'string', project_id: 'project' },
    body: {
      depth: 'int',
      git_connection_id: { kind: 'int', help: "Opaque user-owned connection reference" },
      revision: 'string',
      url: 'string!',
    },
  })

  endpoint(group, restoreApplication, {
    name: "apps restore-application",
    description: "Restore an archived AI application",
    path: { application_public_id: 'string' },
    confirm: "This will restore an archived AI application.",
  })

  endpoint(group, getApplicationWorkspace, {
    name: "apps get-application-workspace",
    description: "Get application workspace",
    path: { application_public_id: 'string' },
  })

  endpoint(group, updateApplicationWorkspace, {
    name: "apps update-application-workspace",
    description: "Update desired application workspace resources",
    path: { application_public_id: 'string' },
    body: {
      cpu_limit: 'number',
      disk_limit_mb: 'int',
      idle_timeout_secs: 'int',
      image: { kind: 'string', help: "Pinned Temps-managed daemon image" },
      memory_limit_mb: 'int',
      pids_limit: 'int',
      runtime: 'string',
    },
  })

  endpoint(group, controlApplicationWorkspace, {
    name: "apps control-application-workspace",
    description: "Control an application workspace",
    path: { application_public_id: 'string' },
    body: {
      action: { kind: 'string!', help: "restart, pause, resume, rebuild, snapshot, restore, or update_runtime" },
      confirm: { kind: 'bool', help: "Must be true for update_runtime because compute is replaced" },
      label: 'string',
      runtime: { kind: 'string', help: "Optional built-in flavor to select during update_runtime" },
      snapshot_id: 'string',
    },
  })

  endpoint(group, getApplicationWorkspaceChanges, {
    name: "apps get-application-workspace-changes",
    description: "Get application workspace changes",
    path: { application_public_id: 'string' },
    query: {
      cursor: { kind: 'int', help: "Position returned by the previous page" },
      limit: { kind: 'int', help: "Files per page (1-200, default 100)" },
    },
  })

  endpoint(group, getApplicationWorkspaceDiff, {
    name: "apps get-application-workspace-diff",
    description: "Get application workspace diff",
    path: { application_public_id: 'string' },
    query: {
      path: 'string!',
    },
  })

  endpoint(group, getApplicationWorkspaceDirectory, {
    name: "apps get-application-workspace-directory",
    description: "Get application workspace directory",
    path: { application_public_id: 'string' },
    query: {
      path: { kind: 'string', help: "Workspace-relative directory; omit for the root" },
      cursor: { kind: 'int', help: "Position returned by the previous page" },
      limit: { kind: 'int', help: "Entries per page (1-100, default 100)" },
    },
  })

  endpoint(group, getApplicationWorkspaceFile, {
    name: "apps get-application-workspace-file",
    description: "Get application workspace file",
    path: { application_public_id: 'string' },
    query: {
      path: 'string!',
    },
  })

  endpoint(group, downloadApplicationWorkspaceFile, {
    name: "apps download-application-workspace-file",
    description: "Download one application workspace file",
    path: { application_public_id: 'string' },
    query: {
      path: 'string!',
    },
    output: 'bytes',
  })

  endpoint(group, uploadApplicationWorkspaceFiles, {
    name: "apps upload-application-workspace-files",
    description: "Upload files into an application workspace",
    path: { application_public_id: 'string' },
    body: {
      files: 'json!',
    },
  })

  endpoint(group, attachApplicationWorkspaceTerminal, {
    name: "apps attach-application-workspace-terminal",
    description: "Attach an interactive terminal to an application workspace",
    path: { application_public_id: 'string' },
    query: {
      thread: { kind: 'string!', help: "Public id of the terminal thread to attach to" },
      cols: { kind: 'int', help: "Initial terminal width in columns (default 80)" },
      rows: { kind: 'int', help: "Initial terminal height in rows (default 24)" },
    },
    output: 'terminal',
  })

  endpoint(group, getBlueprint, {
    name: "apps blueprint get",
    description: "The workspace's architecture blueprint with the live state of its parts",
    path: { public_id: 'string' },
  })

  endpoint(group, unbindBlueprintComponent, {
    name: "apps blueprint unbind-component",
    description: "Remove a part's binding",
    path: { public_id: 'string', component_id: 'string' },
    confirm: "This will remove a part's binding.",
  })

  endpoint(group, bindBlueprintComponent, {
    name: "apps blueprint bind-component",
    description: "Bind a part of the accepted blueprint to the project or service that realises it",
    path: { public_id: 'string', component_id: 'string' },
    body: {
      project_id: 'int',
      service_id: 'int',
    },
  })

  endpoint(group, listBlueprintVersions, {
    name: "apps blueprint list-versions",
    description: "Every version of the workspace's blueprint, newest first",
    path: { public_id: 'string' },
    query: {
      page: { kind: 'int', help: "1-based page" },
      page_size: { kind: 'int', help: "Default 20, at most 100" },
    },
  })

  endpoint(group, saveBlueprintVersion, {
    name: "apps blueprint save-version",
    description: "Save a version the user wrote",
    path: { public_id: 'string' },
    body: {
      change_summary: 'string!',
      document: { kind: 'json!', help: "The design of an application: what it is for, its parts and its flows" },
    },
  })

  endpoint(group, acceptBlueprintVersion, {
    name: "apps blueprint accept-version",
    description: "Accept a proposed version: every chat of the workspace builds against it",
    path: { public_id: 'string', version: 'int' },
  })

  endpoint(group, rejectBlueprintVersion, {
    name: "apps blueprint reject-version",
    description: "Reject a proposed version",
    path: { public_id: 'string', version: 'int' },
  })

  endpoint(group, createAssistantConversation, {
    name: "chat create-assistant-conversation",
    description: "Start a chat with the Temps assistant",
    body: {
      ai_model: { kind: 'string', help: "Model id to start with; defaults to the provider's default model" },
      ai_provider: { kind: 'string', help: "An AI-gateway provider id ('gateway_key:<id>')" },
      project_id: { kind: 'int', help: "A project the chat is about" },
    },
  })

  endpoint(group, listCodingAgents, {
    name: "coding-agents list",
    description: "Every coding-agent CLI Temps manages, its latest published release, and whether newer releases are..",
    query: {
      refresh: { kind: 'bool', help: "Ask the npm registry again instead of using the six-hour cache (at most once a minute)" },
    },
  })

  endpoint(group, updateCodingAgentSettings, {
    name: "coding-agents update-settings",
    description: "Turn automatic coding-agent updates on or off",
    body: {
      auto_update: 'bool',
    },
  })

  endpoint(group, listAllConversations, {
    name: "chat list-all-conversations",
    description: "List the current user's active conversations across all projects, most-recently-active first, annotated..",
    query: {
      status: { kind: 'string', help: "Conversation lifecycle state (defaults to active)", values: ["active","archived"] },
      scope: { kind: 'string', help: "Limit results to the global workspace, or return every readable context", values: ["all","global"] },
      page: { kind: 'int', help: "Page number (1-indexed)" },
      page_size: { kind: 'int', help: "Number of conversations per page (clamped to 1..=100)" },
      search: { kind: 'string', help: "Keep only chats whose title, or whose project's name or slug, contains this text (case-insensitive,..." },
    },
  })

  endpoint(group, createGlobalConversation, {
    name: "chat create-global-conversation",
    description: "Create a private user-owned operator thread without binding its lifetime or authority to a project",
    body: {
      ai_model: 'string',
      ai_permission_mode: 'string',
      ai_provider: { kind: 'string', help: "A registered development harness" },
      ai_thinking_level: 'string',
      kind: { kind: 'string', help: "'ai' (default) or 'terminal'" },
    },
  })

  endpoint(group, getUserConversation, {
    name: "chat get-user-conversation",
    description: "Load one bounded history page for a private conversation by its owner-facing id",
    path: { public_id: 'string' },
    query: {
      before: { kind: 'string', help: "Opaque next_before cursor returned by the previous page" },
      limit: { kind: 'int', help: "Messages per page (default 50, maximum 100)" },
    },
  })

  endpoint(group, renameUserConversation, {
    name: "chat rename-user-conversation",
    description: "Rename user conversation",
    path: { public_id: 'string' },
    body: {
      title: { kind: 'string!', help: "New human-facing title" },
    },
  })

  endpoint(group, archiveUserConversation, {
    name: "chat archive-user-conversation",
    description: "Archive user conversation",
    path: { public_id: 'string' },
    confirm: "This will archive user conversation.",
  })

  endpoint(group, startAttachmentUpload, {
    name: "chat start-attachment-upload",
    description: "Start uploading a file in chunks, for clients behind a small request-size limit",
    path: { public_id: 'string' },
    body: {
      mime_type: { kind: 'string', help: "Content type the browser reported, if any" },
      name: { kind: 'string!', help: "File name as the user knows it; unsafe characters are replaced" },
      size_bytes: { kind: 'int!', help: "Size of the whole file in bytes" },
    },
  })

  endpoint(group, cancelAttachmentUpload, {
    name: "chat cancel-attachment-upload",
    description: "Abandon an upload and free what it reserved",
    path: { public_id: 'string', upload_id: 'string' },
    confirm: "This will abandon an upload and free what it reserved.",
  })

  endpoint(group, appendAttachmentChunk, {
    name: "chat append-attachment-chunk",
    description: "Append the next chunk of an upload",
    path: { public_id: 'string', upload_id: 'string' },
    query: {
      offset: { kind: 'int!', help: "Where this chunk starts in the file: the bytes received so far" },
    },
    body: {
      chunk: 'file!',
    },
  })

  endpoint(group, completeAttachmentUpload, {
    name: "chat complete-attachment-upload",
    description: "Finish an upload once every byte has arrived",
    path: { public_id: 'string', upload_id: 'string' },
  })

  endpoint(group, uploadUserConversationAttachment, {
    name: "chat upload-user-conversation-attachment",
    description: "Upload user conversation attachment",
    path: { public_id: 'string' },
    body: {
      file: 'file!',
    },
  })

  endpoint(group, getUserConversationAttachment, {
    name: "chat get-user-conversation-attachment",
    description: "Get user conversation attachment",
    path: { public_id: 'string', attachment_id: 'string' },
    query: {
      name: 'string!',
    },
    output: 'bytes',
  })

  endpoint(group, compactConversation, {
    name: "chat compact-conversation",
    description: "Summarize the chat's older messages now",
    path: { public_id: 'string' },
  })

  endpoint(group, getUserConversationDiagnostics, {
    name: "chat get-user-conversation-diagnostics",
    description: "Export the bounded, server-redacted diagnostic state Temps currently owns",
    path: { public_id: 'string' },
    query: {
      limit: { kind: 'int', help: "Persisted rows to include, newest window first (default/max 25)" },
    },
  })

  endpoint(group, listGoals, {
    name: "chat list-goals",
    description: "List the goals a chat is watching or has finished with, newest first",
    path: { public_id: 'string' },
  })

  endpoint(group, cancelGoal, {
    name: "chat cancel-goal",
    description: "Stop watching a goal",
    path: { public_id: 'string', goal_id: 'string' },
    confirm: "This will stop watching a goal.",
  })

  endpoint(group, retryGoal, {
    name: "chat retry-goal",
    description: "Give a failed or waiting goal a fresh attempt budget and check it again now",
    path: { public_id: 'string', goal_id: 'string' },
  })

  endpoint(group, sendUserMessage, {
    name: "chat send-user-message",
    description: "Submit a turn to a user-owned conversation",
    path: { public_id: 'string' },
    body: {
      ai_model: { kind: 'string', help: "Optional next-turn model, one advertised by the conversation's provider (or by 'ai_provider' when that is set)" },
      ai_permission_mode: { kind: 'string', help: "Optional next-turn permission mode for the pinned provider harness" },
      ai_provider: { kind: 'string', help: "Optional AI gateway provider ('gateway_key:<id>') for this and later turns" },
      ai_thinking_level: { kind: 'string', help: "Optional next-turn thinking level" },
      attachments: { kind: 'json', help: "Opaque references returned by the conversation attachment endpoint" },
      content: 'string!',
      page_context: { kind: 'string', help: "Optional, client-supplied description of the page/entity the user is currently viewing (e.g. a trace in a..." },
      turn_id: { kind: 'string', help: "Client-generated opaque idempotency key for this turn" },
    },
  })

  endpoint(group, listChatOperations, {
    name: "chat list-operations",
    description: "The deployments, backups and restores this chat started, newest first, with their current state",
    path: { public_id: 'string' },
  })

  endpoint(group, listUserPendingActions, {
    name: "chat list-user-pending-actions",
    description: "List user pending actions",
    path: { public_id: 'string' },
  })

  endpoint(group, updateUserPermissionMode, {
    name: "chat update-user-permission-mode",
    description: "Update user permission mode",
    path: { public_id: 'string' },
    body: {
      permission_mode: { kind: 'string!', help: "Provider permission mode to persist" },
    },
  })

  endpoint(group, resolveUserPermission, {
    name: "chat resolve-user-permission",
    description: "Resolve user permission",
    path: { public_id: 'string', permission_id: 'string' },
    body: {
      decision: { kind: 'json!', help: "The user's decision for a pending permission request" },
    },
  })

  endpoint(group, listChatResources, {
    name: "chat list-resources",
    description: "The projects, databases and services, repositories and sandbox this chat made, with their current state",
    path: { public_id: 'string' },
  })

  endpoint(group, restoreUserConversation, {
    name: "chat restore-user-conversation",
    description: "Restore a user-owned archived conversation",
    path: { public_id: 'string' },
    confirm: "This will restore a user-owned archived conversation.",
  })

  endpoint(group, unlinkChatSandbox, {
    name: "chat unlink-sandbox",
    description: "Unlink the chat's sandbox",
    path: { public_id: 'string' },
    confirm: "This will unlink the chat's sandbox.",
  })

  endpoint(group, getChatSandbox, {
    name: "chat get-sandbox",
    description: "Whether the chat can use a sandbox, and which one it is linked to",
    path: { public_id: 'string' },
  })

  endpoint(group, linkChatSandbox, {
    name: "chat link-sandbox",
    description: "Link a sandbox to the chat, or create one for it",
    path: { public_id: 'string' },
    body: {
      create: { kind: 'bool', help: "Create a fresh sandbox for this chat instead" },
      sandbox_id: { kind: 'string', help: "Public id of one of the caller's own sandboxes" },
    },
  })

  endpoint(group, getChatSandboxChanges, {
    name: "chat get-sandbox-changes",
    description: "The uncommitted Git changes of every repository under the sandbox work directory",
    path: { public_id: 'string' },
  })

  endpoint(group, getChatSandboxDiff, {
    name: "chat get-sandbox-diff",
    description: "Both sides of one changed file: as last committed, and as it is now",
    path: { public_id: 'string' },
    query: {
      repo: { kind: 'string!', help: "The repository root, absolute or relative to the work directory" },
      path: { kind: 'string!', help: "The file, relative to the repository root" },
      old_path: { kind: 'string', help: "The path before a rename, relative to the repository root" },
    },
  })

  endpoint(group, trackChatSandboxChanges, {
    name: "chat track-sandbox-changes",
    description: "Put a folder of the sandbox under Git, so its changes show from now on: a '.gitignore' for dependencies,..",
    path: { public_id: 'string' },
    body: {
      path: { kind: 'string', help: "Absolute, or relative to the work directory; the work directory when omitted" },
    },
  })

  endpoint(group, readChatSandboxFile, {
    name: "chat read-sandbox-file",
    description: "One file of the chat's sandbox, for the viewer",
    path: { public_id: 'string' },
    query: {
      path: { kind: 'string!', help: "Absolute, or relative to the sandbox work directory" },
    },
  })

  endpoint(group, listChatSandboxFiles, {
    name: "chat list-sandbox-files",
    description: "List one directory of the chat's sandbox: folders first, then files",
    path: { public_id: 'string' },
    query: {
      path: { kind: 'string', help: "Absolute, or relative to the sandbox work directory" },
    },
  })

  endpoint(group, connectChatSandboxProject, {
    name: "chat connect-sandbox",
    description: "Let the chat's sandbox use a project's database",
    path: { public_id: 'string' },
    body: {
      project_id: { kind: 'int!', help: "The project whose services the sandbox may use" },
    },
  })

  endpoint(group, disconnectChatSandboxProject, {
    name: "chat disconnect-sandbox",
    description: "Stop the chat's sandbox using a project's database",
    path: { public_id: 'string', project_id: 'project' },
    confirm: "This will stop the chat's sandbox using a project's database.",
  })

  endpoint(group, connectChatSandboxService, {
    name: "chat connect-sandbox-service",
    description: "Let the chat's sandbox use one service, whether or not it belongs to a project",
    path: { public_id: 'string' },
    body: {
      project_id: { kind: 'int', help: "A project the service is linked to" },
      service_id: { kind: 'int!', help: "The service the sandbox may use" },
    },
  })

  endpoint(group, disconnectChatSandboxService, {
    name: "chat disconnect-sandbox-service",
    description: "Stop the chat's sandbox using one service",
    path: { public_id: 'string', service_id: 'int' },
    confirm: "This will stop the chat's sandbox using one service.",
  })

  endpoint(group, wakeChatSandbox, {
    name: "chat wake-sandbox",
    description: "Start the chat's sandbox if it went to sleep, so the preview and the assistant's next command find it up",
    path: { public_id: 'string' },
  })

  endpoint(group, listSecretRequests, {
    name: "chat list-secret-requests",
    description: "List the credentials this chat has asked for and not yet used up or lost",
    path: { public_id: 'string' },
  })

  endpoint(group, provideSecret, {
    name: "chat provide-secret",
    description: "Provide the credential the assistant asked for",
    path: { public_id: 'string', secret_id: 'string' },
    body: {
      value: { kind: 'secret!', help: "The credential" },
    },
  })

  endpoint(group, discardSecret, {
    name: "chat discard-secret",
    description: "Discard a credential request and anything already typed into it",
    path: { public_id: 'string', secret_id: 'string' },
    confirm: "This will discard a credential request and anything already typed into it.",
  })

  endpoint(group, stopUserTurn, {
    name: "chat stop-user-turn",
    description: "Stop user turn",
    path: { public_id: 'string' },
  })

  endpoint(group, getChatSuggestionMode, {
    name: "chat get-suggestion-mode",
    description: "A chat's next-step suggestion setting, with the default it falls back to",
    path: { public_id: 'string' },
  })

  endpoint(group, updateChatSuggestionMode, {
    name: "chat update-suggestion-mode",
    description: "Set a chat's own next-step suggestion mode, or clear it to follow the user's default",
    path: { public_id: 'string' },
    body: {
      mode: { kind: 'string', help: "What happens after each answer", values: ["off","ask","always"] },
    },
  })

  endpoint(group, suggestNextSteps, {
    name: "chat suggest-next-steps",
    description: "Suggest a few next steps after the chat's latest answer",
    path: { public_id: 'string' },
    body: {
      answer: { kind: 'string', help: "Cursor of the answer the user is looking at, from the message list" },
    },
  })

  endpoint(group, getUserConversationUsage, {
    name: "chat get-user-conversation-usage",
    description: "What this conversation has cost so far",
    path: { public_id: 'string' },
  })

  endpoint(group, leaveChatWorkspace, {
    name: "chat leave-workspace",
    description: "Take this chat out of its workspace",
    path: { public_id: 'string' },
    confirm: "This will take this chat out of its workspace.",
  })

  endpoint(group, getChatWorkspace, {
    name: "chat get-workspace",
    description: "The workspace this chat is in, with its other chats",
    path: { public_id: 'string' },
  })

  endpoint(group, setChatWorkspace, {
    name: "chat set-workspace",
    description: "Put this chat in a workspace, creating it when asked",
    path: { public_id: 'string' },
    body: {
      application_id: 'string',
      create: { kind: 'json', help: "A new workspace to create for the chat" },
      keep_sandbox: { kind: 'string', help: "Which sandbox the chats keep when both the chat and the workspace have one", values: ["workspace","chat"] },
    },
  })

  endpoint(group, getLimitsOverview, {
    name: "gateway limits get-overview",
    description: "Get limits overview",
    query: {
      page: { kind: 'int', help: "Page of project overrides (default 1)" },
      page_size: { kind: 'int', help: "Page size (default 20, max 100)" },
    },
  })

  endpoint(group, updateInstanceLimits, {
    name: "gateway limits update-instance",
    description: "Update instance limits",
    body: {
      max_cost_per_month_microcents: { kind: 'int', help: "Operator-funded spend per UTC calendar month, in microcents (1/10000 of a cent; 1 USD = 1,000,000 microcents)" },
      max_requests_per_minute: { kind: 'int', help: "Requests per rolling minute" },
    },
  })

  endpoint(group, resetProjectLimits, {
    name: "gateway limits reset",
    description: "Reset project limits",
    path: { project_id: 'project' },
    confirm: "This will reset project limits.",
  })

  endpoint(group, getProjectLimits, {
    name: "gateway limits get",
    description: "Get project limits",
    path: { project_id: 'project' },
  })

  endpoint(group, updateProjectLimits, {
    name: "gateway limits update",
    description: "Update project limits",
    path: { project_id: 'project' },
    body: {
      max_cost_per_month_microcents: { kind: 'int', help: "Operator-funded spend per UTC calendar month, in microcents (1/10000 of a cent; 1 USD = 1,000,000 microcents)" },
      max_requests_per_minute: { kind: 'int', help: "Requests per rolling minute" },
    },
  })

  endpoint(group, getModelCatalog, {
    name: "gateway catalog get-model",
    description: "The runtime model catalog's settings and the outcome of its last refresh",
  })

  endpoint(group, listModelCatalogEntries, {
    name: "gateway catalog list-model-entries",
    description: "The stored catalog's models, newest release first",
    query: {
      provider: { kind: 'string', help: "Temps provider id ('openai', 'anthropic', 'gemini', 'xai', 'openrouter')" },
      search: { kind: 'string', help: "Case-insensitive part of the model id or name" },
      page: { kind: 'int', help: "Page number, from 1 (default 1)" },
      page_size: { kind: 'int', help: "Page size (default 20, max 100)" },
    },
  })

  endpoint(group, refreshModelCatalog, {
    name: "gateway catalog refresh-model",
    description: "Fetch the catalog now, store it and apply its prices",
  })

  endpoint(group, updateModelCatalogSettings, {
    name: "gateway catalog update-model-settings",
    description: "Change where the catalog comes from and how it is refreshed",
    body: {
      auto_refresh: 'bool',
      discover_provider_models: 'bool',
      refresh_interval_hours: 'int',
      source_url: { kind: 'string', help: "A custom 'https://' catalog URL, or an empty string to go back to the catalog published in the Temps..." },
    },
  })

  endpoint(group, listUnpricedModels, {
    name: "gateway catalog list-unpriced-models",
    description: "Models enabled on a provider key that no price covers",
  })

  endpoint(group, getUserPendingAction, {
    name: "chat get-user-pending-action",
    description: "Get user pending action",
    path: { action_public_id: 'string' },
  })

  endpoint(group, confirmUserPendingAction, {
    name: "chat confirm-user-pending-action",
    description: "Confirm user pending action",
    path: { action_public_id: 'string' },
  })

  endpoint(group, rejectUserPendingAction, {
    name: "chat reject-user-pending-action",
    description: "Reject user pending action",
    path: { action_public_id: 'string' },
  })

  endpoint(group, getSuggestionDefault, {
    name: "chat get-suggestion-default",
    description: "The user's default for next-step suggestions",
  })

  endpoint(group, updateSuggestionDefault, {
    name: "chat update-suggestion-default",
    description: "Set the user's default for every chat that does not set its own mode",
    body: {
      mode: { kind: 'string!', help: "What happens after each answer", values: ["off","ask","always"] },
    },
  })

  endpoint(group, getPricing, {
    name: "gateway pricing get",
    description: "Get pricing",
  })

  endpoint(group, getAiProviderStatus, {
    name: "provider-status get",
    description: "Get ai provider status",
  })

  endpoint(group, refreshAiProviderStatus, {
    name: "provider-status refresh",
    description: "Refresh ai provider status",
  })

  endpoint(group, listProviderKeys, {
    name: "gateway providers list-keys",
    description: "List provider keys",
  })

  endpoint(group, createProviderKey, {
    name: "gateway providers create-key",
    description: "Create provider key",
    body: {
      api_key: { kind: 'secret', help: "Provider API key" },
      base_url: { kind: 'string', help: "Required for \"openai_compatible\", e.g. \"http://10.0.0.5:8000/v1\"" },
      capabilities: { kind: 'json', help: "Capabilities an administrator declared for a self-hosted key" },
      default_model: { kind: 'string', help: "Model id to pin for this provider (e.g. \"gpt-6.2-sol\")" },
      display_name: 'string!',
      provider: { kind: 'string!', help: "\"openai\", \"anthropic\", \"xai\", \"gemini\", \"openrouter\", or \"openai_compatible\" for a self-hosted..." },
      trust_private_address: { kind: 'bool', help: "Trust the exact private 'ip:port' of 'base_url' for this key only, without adding it to the instance-wide..." },
    },
  })

  endpoint(group, discoverProviderModels, {
    name: "gateway providers discover-models",
    description: "Discover provider models",
    body: {
      api_key: { kind: 'secret', help: "API key, if the server requires one" },
      base_url: { kind: 'string', help: "Base URL of the server (required for \"openai_compatible\")" },
      provider: { kind: 'string!', help: "Provider ID, e.g. \"openai_compatible\"" },
      trust_private_address: { kind: 'bool', help: "Discover as if the base URL's exact private address were trusted on the key" },
    },
  })

  endpoint(group, testProviderKeyInline, {
    name: "gateway providers test-key-inline",
    description: "Test provider key inline",
    body: {
      api_key: { kind: 'secret', help: "The raw API key to test (may be empty for a self-hosted provider)" },
      base_url: { kind: 'string', help: "Optional custom base URL (required for \"openai_compatible\")" },
      default_model: { kind: 'string', help: "Model to send the test completion to" },
      provider: { kind: 'string!', help: "Provider ID: \"openai\", \"anthropic\", \"xai\", \"gemini\", \"openrouter\", \"openai_compatible\"" },
      trust_private_address: { kind: 'bool', help: "Test as if the base URL's exact private address were trusted on the key" },
    },
  })

  endpoint(group, deleteProviderKey, {
    name: "gateway providers delete-key",
    description: "Delete provider key",
    path: { id: 'int' },
    confirm: "This will delete provider key.",
  })

  endpoint(group, getProviderKey, {
    name: "gateway providers get-key",
    description: "Get provider key",
    path: { id: 'int' },
  })

  endpoint(group, updateProviderKey, {
    name: "gateway providers update-key",
    description: "Update provider key",
    path: { id: 'int' },
    body: {
      api_key: { kind: 'secret', help: "A credential" },
      base_url: { kind: 'string', help: "Double-Option: absent = leave unchanged, present-null = clear (revert to the provider's default endpoint),..." },
      capabilities: { kind: 'json', help: "Capabilities an administrator declared for a self-hosted key" },
      default_model: { kind: 'string', help: "Double-Option: absent = leave unchanged, present-null = clear the pinned model (revert to the per-provider..." },
      display_name: 'string',
      is_active: 'bool',
      trust_private_address: { kind: 'bool', help: "'true' trusts the exact private 'ip:port' of the (new) base URL for this key only (requires..." },
    },
  })

  endpoint(group, addProviderModel, {
    name: "gateway providers add-model",
    description: "Add provider model",
    path: { id: 'int' },
    body: {
      display_name: 'string',
      model_id: 'string!',
    },
  })

  endpoint(group, refreshProviderModels, {
    name: "gateway providers refresh-models",
    description: "Refresh provider models",
    path: { id: 'int' },
  })

  endpoint(group, deleteProviderModel, {
    name: "gateway providers delete-model",
    description: "Delete provider model",
    path: { id: 'int', model_row_id: 'int' },
    confirm: "This will delete provider model.",
  })

  endpoint(group, updateProviderModel, {
    name: "gateway providers update-model",
    description: "Update provider model",
    path: { id: 'int', model_row_id: 'int' },
    body: {
      is_enabled: 'bool!',
    },
  })

  endpoint(group, testProviderKeyById, {
    name: "gateway providers test-key-by-id",
    description: "Test provider key by id",
    path: { id: 'int' },
  })

  endpoint(group, updateAiSummaryPreference, {
    name: "provider-status update-summary-preference",
    description: "Update ai summary preference",
    body: {
      model: 'string',
      provider_id: 'string',
      thinking_level: 'string',
    },
  })

  endpoint(group, getUsageByProject, {
    name: "gateway usage get-by",
    description: "Get usage by project",
    query: {
      from: { kind: 'string', help: "ISO 8601 start time (defaults to 24h ago)" },
      to: { kind: 'string', help: "ISO 8601 end time (defaults to now)" },
      limit: { kind: 'int', help: "Max projects (defaults to 50, max 100)" },
    },
  })

  endpoint(group, getUsageByProvider, {
    name: "gateway usage get-by-provider",
    description: "Get usage by provider",
    query: {
      from: { kind: 'string', help: "ISO 8601 start time (defaults to 24h ago)" },
      to: { kind: 'string', help: "ISO 8601 end time (defaults to now)" },
      project_id: { kind: 'int', help: "Filter by project ID (deployment-token usage; instance administrators only)" },
    },
  })

  endpoint(group, getConversations, {
    name: "gateway usage get-conversations",
    description: "Get conversations",
    query: {
      project_id: { kind: 'int', help: "Filter by project ID (deployment-token usage; instance administrators only)" },
      from: { kind: 'string', help: "ISO 8601 start time (defaults to 24h ago)" },
      to: { kind: 'string', help: "ISO 8601 end time (defaults to now)" },
      limit: { kind: 'int', help: "Max results (defaults to 50, max 100)" },
      user_id: { kind: 'int', help: "Filter by user ID" },
      tags: { kind: 'string', help: "Filter by tags (comma-separated)" },
      model: { kind: 'string', help: "Filter by model name" },
    },
  })

  endpoint(group, getConversationDetail, {
    name: "gateway usage get-conversation-detail",
    description: "Get conversation detail",
    path: { conversation_id: 'string' },
    query: {
      limit: { kind: 'int', help: "Max results (defaults to 100)" },
    },
  })

  endpoint(group, getUsageRecent, {
    name: "gateway usage get-recent",
    description: "Get usage recent",
    query: {
      limit: { kind: 'int', help: "Page size (defaults to 20, max 50)" },
      offset: { kind: 'int', help: "Number of results to skip for pagination (defaults to 0)" },
      provider: { kind: 'string', help: "Filter by provider name" },
      model: { kind: 'string', help: "Filter by model name" },
      project_id: { kind: 'int', help: "Filter by project ID (deployment-token usage; instance administrators only)" },
      status: { kind: 'int', help: "Filter by HTTP status code (exact match)" },
      cost_gte: { kind: 'int', help: "Cost greater-than-or-equal, in microcents" },
      cost_gt: { kind: 'int', help: "Cost strictly greater-than, in microcents" },
      cost_lte: { kind: 'int', help: "Cost less-than-or-equal, in microcents" },
      cost_lt: { kind: 'int', help: "Cost strictly less-than, in microcents" },
      tokens_gte: { kind: 'int', help: "Total tokens greater-than-or-equal" },
      tokens_gt: { kind: 'int', help: "Total tokens strictly greater-than" },
      tokens_lte: { kind: 'int', help: "Total tokens less-than-or-equal" },
      tokens_lt: { kind: 'int', help: "Total tokens strictly less-than" },
    },
  })

  endpoint(group, getUsageSummary, {
    name: "gateway usage get-summary",
    description: "Get usage summary",
    query: {
      from: { kind: 'string', help: "ISO 8601 start time (defaults to 24h ago)" },
      to: { kind: 'string', help: "ISO 8601 end time (defaults to now)" },
      project_id: { kind: 'int', help: "Filter by project ID (deployment-token usage; instance administrators only)" },
    },
  })

  endpoint(group, getUsageTimeseries, {
    name: "gateway usage get-timeseries",
    description: "Get usage timeseries",
    query: {
      from: { kind: 'string', help: "ISO 8601 start time (defaults to 24h ago)" },
      to: { kind: 'string', help: "ISO 8601 end time (defaults to now)" },
      bucket: { kind: 'string', help: "Bucket size: hour, day, week (defaults to day)" },
      project_id: { kind: 'int', help: "Filter by project ID (deployment-token usage; instance administrators only)" },
    },
  })

  endpoint(group, getUsageTopModels, {
    name: "gateway usage get-top-models",
    description: "Get usage top models",
    query: {
      from: { kind: 'string', help: "ISO 8601 start time (defaults to 24h ago)" },
      to: { kind: 'string', help: "ISO 8601 end time (defaults to now)" },
      limit: { kind: 'int', help: "Max results (defaults to 10)" },
      project_id: { kind: 'int', help: "Filter by project ID (deployment-token usage; instance administrators only)" },
    },
  })

  endpoint(group, createBatch, {
    name: "gateway create-batch",
    description: "Create batch",
    body: {
      completion_window: { kind: 'string!', help: "Currently only '24h'" },
      endpoint: { kind: 'string!', help: "Endpoint every request in the input file targets: '/v1/responses', '/v1/chat/completions' or '/v1/embeddings'" },
      input_file_id: { kind: 'string!', help: "A file uploaded through 'POST /ai/v1/files' by the same caller" },
      metadata: 'json',
    },
  })

  endpoint(group, retrieveBatch, {
    name: "gateway retrieve-batch",
    description: "Retrieve batch",
    path: { batch_id: 'string' },
  })

  endpoint(group, cancelBatch, {
    name: "gateway cancel-batch",
    description: "Cancel batch",
    path: { batch_id: 'string' },
    confirm: "This will cancel batch.",
  })

  endpoint(group, chatCompletions, {
    name: "gateway chat-completions",
    description: "Chat completions",
    body: {
      frequency_penalty: 'number',
      max_tokens: 'int',
      messages: 'json!',
      model: 'string!',
      n: 'int',
      presence_penalty: 'number',
      response_format: 'json',
      seed: 'int',
      stop: 'json',
      stream: 'bool',
      temperature: 'number',
      tool_choice: 'json',
      tools: 'json',
      top_p: 'number',
      user: 'string',
    },
  })

  endpoint(group, embeddings, {
    name: "gateway embeddings",
    description: "Embeddings",
    body: {
      dimensions: 'int',
      encoding_format: 'string',
      input: 'json!',
      model: 'string!',
    },
  })

  endpoint(group, uploadFile, {
    name: "gateway upload-file",
    description: "Upload file",
    body: {
      file: { kind: 'file!', help: "A JSONL batch input file: one request per line, every line targeting the same model and endpoint" },
      purpose: { kind: 'string!', help: "Must be 'batch'" },
    },
  })

  endpoint(group, deleteFile, {
    name: "gateway delete-file",
    description: "Delete file",
    path: { file_id: 'string' },
    confirm: "This will delete file.",
  })

  endpoint(group, retrieveFile, {
    name: "gateway retrieve-file",
    description: "Retrieve file",
    path: { file_id: 'string' },
  })

  endpoint(group, fileContent, {
    name: "gateway file-content",
    description: "File content",
    path: { file_id: 'string' },
    output: 'bytes',
  })

  endpoint(group, listModels, {
    name: "gateway list-models",
    description: "List models",
  })

  endpoint(group, createResponseJson, {
    name: "gateway create-response-json",
    description: "Explicit JSON contract for generated clients; the standard OpenAI route remains polymorphic",
    body: {
      background: { kind: 'bool', help: "Run asynchronously and poll for the result" },
      conversation: { kind: 'json', help: "Attach the response to a stored conversation" },
      model: { kind: 'string!', help: "Model to run, e.g. 'gpt-6-luna'" },
      previous_response_id: { kind: 'string', help: "Continue from a stored response" },
      stream: { kind: 'bool', help: "Stream the reply as server-sent events" },
    },
  })

  endpoint(group, createResponseStream, {
    name: "gateway create-response-stream",
    description: "Explicit SSE contract for generated clients; never buffers a successful event stream",
    body: {
      background: { kind: 'bool', help: "Run asynchronously and poll for the result" },
      conversation: { kind: 'json', help: "Attach the response to a stored conversation" },
      model: { kind: 'string!', help: "Model to run, e.g. 'gpt-6-luna'" },
      previous_response_id: { kind: 'string', help: "Continue from a stored response" },
      stream: { kind: 'bool', help: "Stream the reply as server-sent events" },
    },
    output: 'events',
  })

  endpoint(group, getGlobalAiWorkspace, {
    name: "workspace get-global",
    description: "Get global ai workspace",
  })

  endpoint(group, getWorkspaceActivity, {
    name: "workspace get-activity",
    description: "Aggregate all active threads for the caller's global workspace and up to 100 requested, visible..",
    query: {
      application_public_ids: { kind: 'string', help: "Comma-separated application public IDs (maximum 100)" },
    },
  })

  endpoint(group, getGlobalWorkspaceChanges, {
    name: "workspace get-global-changes",
    description: "Get global workspace changes",
    query: {
      cursor: { kind: 'int', help: "Position returned by the previous page" },
      limit: { kind: 'int', help: "Files per page (1-200, default 100)" },
    },
  })

  endpoint(group, getGlobalWorkspaceDiff, {
    name: "workspace get-global-diff",
    description: "Get global workspace diff",
    query: {
      path: 'string!',
    },
  })

  endpoint(group, getGlobalWorkspaceDirectory, {
    name: "workspace get-global-directory",
    description: "Get global workspace directory",
    query: {
      path: { kind: 'string', help: "Workspace-relative directory; omit for the root" },
      cursor: { kind: 'int', help: "Position returned by the previous page" },
      limit: { kind: 'int', help: "Entries per page (1-100, default 100)" },
    },
  })

  endpoint(group, getGlobalWorkspaceFile, {
    name: "workspace get-global-file",
    description: "Get global workspace file",
    query: {
      path: 'string!',
    },
  })

  endpoint(group, getWorkspaceFileLimits, {
    name: "workspace get-file-limits",
    description: "Get effective AI workspace file limits",
  })

  endpoint(group, downloadGlobalWorkspaceFile, {
    name: "workspace download-global-file",
    description: "Download one global AI workspace file",
    query: {
      path: 'string!',
    },
    output: 'bytes',
  })

  endpoint(group, uploadGlobalWorkspaceFiles, {
    name: "workspace upload-global-files",
    description: "Upload files into the global AI workspace",
    body: {
      files: 'json!',
    },
  })

  endpoint(group, createGlobalWorkspacePreviewLink, {
    name: "workspace create-global-preview-link",
    description: "Mint a short-lived preview URL for a port in the user's shared global AI workspace",
    body: {
      path: { kind: 'string', help: "Optional same-origin path to open after the preview grant is exchanged" },
      port: { kind: 'int!', help: "The development server port detected or selected in the sandbox" },
    },
  })

  endpoint(group, attachGlobalWorkspaceTerminal, {
    name: "workspace attach-global-terminal",
    description: "Attach an interactive terminal to the user's global workspace",
    query: {
      thread: { kind: 'string!', help: "Public id of the terminal thread to attach to" },
      cols: { kind: 'int', help: "Initial terminal width in columns (default 80)" },
      rows: { kind: 'int', help: "Initial terminal height in rows (default 24)" },
    },
    output: 'terminal',
  })

  endpoint(group, findConversation, {
    name: "chat find-conversation",
    description: "Find the current user's existing chat for a context (returns 'null' if none yet)",
    path: { project_id: 'project' },
    query: {
      context_type: 'string!',
      context_id: 'string!',
    },
  })

  endpoint(group, createConversation, {
    name: "chat create-conversation",
    description: "Get-or-create the current user's private chat for a context",
    path: { project_id: 'project' },
    body: {
      ai_model: 'string',
      ai_permission_mode: 'string',
      ai_provider: { kind: 'string', help: "Provider pinned to this conversation" },
      ai_thinking_level: 'string',
      context_id: { kind: 'string!', help: "The entity id (ints stringified)" },
      context_type: { kind: 'string!', help: "e.g. '\"deployment\"'" },
    },
  })

  endpoint(group, listConversations, {
    name: "chat list-conversations",
    description: "List the current user's active conversations for a project, most-recently-active first",
    path: { project_id: 'project' },
  })

  endpoint(group, getConversation, {
    name: "chat get-conversation",
    description: "One bounded conversation-history page (excluding internal context rows)",
    path: { project_id: 'project', public_id: 'string' },
    query: {
      before: { kind: 'string', help: "Opaque next_before cursor returned by the previous page" },
      limit: { kind: 'int', help: "Messages per page (default 50, maximum 100)" },
    },
  })

  endpoint(group, renameConversation, {
    name: "chat rename-conversation",
    description: "Rename a conversation (set its human-facing title)",
    path: { project_id: 'project', public_id: 'string' },
    body: {
      title: { kind: 'string!', help: "New human-facing title" },
    },
  })

  endpoint(group, archiveConversation, {
    name: "chat archive-conversation",
    description: "Archive (soft-delete) a conversation",
    path: { project_id: 'project', public_id: 'string' },
    confirm: "This will archive (soft-delete) a conversation.",
  })

  endpoint(group, sendProjectAiMessage, {
    name: "chat send-message",
    description: "Submit a user message and start a server-owned turn",
    path: { project_id: 'project', public_id: 'string' },
    body: {
      ai_model: { kind: 'string', help: "Optional next-turn model, one advertised by the conversation's provider (or by 'ai_provider' when that is set)" },
      ai_permission_mode: { kind: 'string', help: "Optional next-turn permission mode for the pinned provider harness" },
      ai_provider: { kind: 'string', help: "Optional AI gateway provider ('gateway_key:<id>') for this and later turns" },
      ai_thinking_level: { kind: 'string', help: "Optional next-turn thinking level" },
      attachments: { kind: 'json', help: "Opaque references returned by the conversation attachment endpoint" },
      content: 'string!',
      page_context: { kind: 'string', help: "Optional, client-supplied description of the page/entity the user is currently viewing (e.g. a trace in a..." },
      turn_id: { kind: 'string', help: "Client-generated opaque idempotency key for this turn" },
    },
  })

  endpoint(group, listPendingActions, {
    name: "chat list-pending-actions",
    description: "List all pending actions for a conversation (most-recently-proposed first)",
    path: { project_id: 'project', public_id: 'string' },
  })

  endpoint(group, updatePermissionMode, {
    name: "chat update-permission-mode",
    description: "Update permission mode",
    path: { project_id: 'project', public_id: 'string' },
    body: {
      permission_mode: { kind: 'string!', help: "Provider permission mode to persist" },
    },
  })

  endpoint(group, resolvePermission, {
    name: "chat resolve-permission",
    description: "Resolve permission",
    path: { project_id: 'project', public_id: 'string', permission_id: 'string' },
    body: {
      decision: { kind: 'json!', help: "The user's decision for a pending permission request" },
    },
  })

  endpoint(group, stopTurn, {
    name: "chat stop-turn",
    description: "Explicitly cancel the server-owned active turn",
    path: { project_id: 'project', public_id: 'string' },
  })

  endpoint(group, getPendingAction, {
    name: "chat get-pending-action",
    description: "Get a single pending action by its public id (scoped to the project)",
    path: { project_id: 'project', action_public_id: 'string' },
  })

  endpoint(group, confirmPendingAction, {
    name: "chat confirm-pending-action",
    description: "Confirm a proposed AI action: validate permission, atomically claim, execute, persist outcome",
    path: { project_id: 'project', action_public_id: 'string' },
  })

  endpoint(group, rejectPendingAction, {
    name: "chat reject-pending-action",
    description: "Reject a proposed AI action (no execution)",
    path: { project_id: 'project', action_public_id: 'string' },
  })

  endpoint(group, streamStructuredOutput, {
    name: "gateway stream-structured-output",
    description: "Stream structured output",
    path: { project_id: 'project' },
    body: {
      cache_key: { kind: 'string', help: "Optional screen-owned cache identity" },
      cache_ttl_seconds: { kind: 'int', help: "Backend cache lifetime in seconds (default 60, clamped to 1-300)" },
      model: { kind: 'string', help: "Optional enabled model id for the selected key" },
      prompt: { kind: 'string!', help: "The complete, already-authorized context and instructions for the model" },
      provider_key_id: { kind: 'int', help: "Optional configured gateway key" },
      purpose: { kind: 'string!', help: "Stable product use-case identifier, for example 'api_traffic.summary'" },
      refresh: { kind: 'bool', help: "Bypass any matching backend cache entry and replace it with this result" },
      schema: { kind: 'json!', help: "JSON Schema describing the required response object" },
      thinking_level: { kind: 'string', help: "Optional normalized reasoning depth advertised for the selected model" },
    },
    output: 'events',
  })
}
