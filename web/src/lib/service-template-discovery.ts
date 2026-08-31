// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export type ServiceCategoryIcon =
  | 'ai'
  | 'automation'
  | 'communication'
  | 'database'
  | 'developer'
  | 'finance'
  | 'media'
  | 'monitoring'
  | 'productivity'
  | 'security'
  | 'storage'
  | 'generic'

export function serviceCategoryIcon(category: string): ServiceCategoryIcon {
  const normalized = category.trim().toLowerCase()
  const tokens = new Set(normalized.split(/[^a-z0-9]+/).filter(Boolean))

  if (tokens.has('ai') || normalized.includes('machine learning')) return 'ai'
  if (normalized.includes('automat')) return 'automation'
  if (
    normalized.includes('communication') ||
    normalized.includes('chat') ||
    normalized.includes('social')
  )
    return 'communication'
  if (normalized.includes('database')) return 'database'
  if (
    normalized.includes('developer') ||
    normalized.includes('devtool') ||
    normalized.includes('code')
  )
    return 'developer'
  if (normalized.includes('finance')) return 'finance'
  if (
    normalized.includes('media') ||
    normalized.includes('photo') ||
    normalized.includes('video')
  )
    return 'media'
  if (
    normalized.includes('monitor') ||
    normalized.includes('analytics') ||
    normalized.includes('observability')
  )
    return 'monitoring'
  if (
    normalized.includes('productivity') ||
    normalized.includes('project management')
  )
    return 'productivity'
  if (
    normalized.includes('security') ||
    normalized.includes('authentication') ||
    normalized.includes('identity')
  )
    return 'security'
  if (
    normalized.includes('storage') ||
    normalized.includes('backup') ||
    normalized.includes('file')
  )
    return 'storage'
  return 'generic'
}

export function toggleServiceTag(
  current: string | null,
  next: string
): string | null {
  return current === next ? null : next
}
