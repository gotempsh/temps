// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ToolCall } from './chat-message-parts'

export interface ProjectCollectionItem {
  /** Missing only for model-authored semantic artifacts. Tool receipts always
   * carry an authoritative id, which is required before navigation is enabled. */
  id: number | null
  name: string
  slug: string
  repoName?: string
  repoOwner?: string
  preset?: string
}

export interface ProjectCollectionPresentation {
  items: ProjectCollectionItem[]
  total: number
  page: number
  perPage: number
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function isTempsReadTool(name: string): boolean {
  return name === 'temps' || name === 'mcp__temps-chat__temps'
}

function isTempsWriteTool(name: string): boolean {
  return name === 'temps_write' || name === 'mcp__temps-chat__temps_write'
}

function operationFromArguments(argumentsJson: string): string | null {
  try {
    const value: unknown = JSON.parse(argumentsJson)
    if (!isRecord(value) || typeof value.command !== 'string') return null
    const [section, operation] = value.command.trim().split(/\s+/)
    return section === 'projects' ? (operation ?? null) : null
  } catch {
    return null
  }
}

function projectItem(value: unknown): ProjectCollectionItem | null {
  if (!isRecord(value)) return null
  if (
    typeof value.id !== 'number' ||
    !Number.isSafeInteger(value.id) ||
    value.id <= 0 ||
    typeof value.name !== 'string' ||
    typeof value.slug !== 'string'
  ) {
    return null
  }
  return {
    id: value.id,
    name: value.name,
    slug: value.slug,
    repoName: typeof value.repo_name === 'string' ? value.repo_name : undefined,
    repoOwner:
      typeof value.repo_owner === 'string' ? value.repo_owner : undefined,
    preset: typeof value.preset === 'string' ? value.preset : undefined,
  }
}

/**
 * Select a trusted native presentation from a read-tool receipt. The operation
 * id and response envelope come from Temps; the model never supplies a React
 * component name. Unknown operations deliberately fall back to ToolCard.
 */
export function projectCollectionFromTool(
  tool: ToolCall
): ProjectCollectionPresentation | null {
  if (
    !isTempsReadTool(tool.name) ||
    operationFromArguments(tool.arguments) !== 'get_projects' ||
    !tool.result
  ) {
    return null
  }

  try {
    const envelope: unknown = JSON.parse(tool.result)
    if (!isRecord(envelope)) return null
    if (envelope.operation !== 'get_projects') return null
    if (
      typeof envelope.status !== 'number' ||
      envelope.status < 200 ||
      envelope.status >= 300 ||
      !isRecord(envelope.data) ||
      !Array.isArray(envelope.data.projects)
    ) {
      return null
    }

    const items = envelope.data.projects
      .map(projectItem)
      .filter((item): item is ProjectCollectionItem => item !== null)
    return {
      items,
      total:
        typeof envelope.data.total === 'number'
          ? envelope.data.total
          : items.length,
      page: typeof envelope.data.page === 'number' ? envelope.data.page : 1,
      perPage:
        typeof envelope.data.per_page === 'number'
          ? envelope.data.per_page
          : items.length,
    }
  } catch {
    return null
  }
}

/** Render the authoritative application topology returned by the composite
 * create operation. The server response includes the newly-created project
 * after its database link, workspace directory, permissions, and private
 * network membership have all succeeded, so this never presents a partially
 * completed AI plan as a project. */
export function projectCollectionFromApplicationProjectWrite(
  tool: ToolCall
): ProjectCollectionPresentation | null {
  if (!isTempsWriteTool(tool.name) || !tool.result) return null

  try {
    const envelope: unknown = JSON.parse(tool.result)
    if (
      !isRecord(envelope) ||
      envelope.status !== 'executed' ||
      envelope.operation !== 'create_application_project' ||
      !isRecord(envelope.result) ||
      !Array.isArray(envelope.result.projects)
    ) {
      return null
    }

    const items = envelope.result.projects
      .map(projectItem)
      .filter((item): item is ProjectCollectionItem => item !== null)
    if (items.length === 0) return null

    return {
      items,
      total: items.length,
      page: 1,
      perPage: items.length,
    }
  } catch {
    return null
  }
}
