// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import { buildOperationOptions } from './AuditLogs'

describe('audit operation filters', () => {
  test('offers plugin permission and activity filters', () => {
    const options = buildOperationOptions()
    expect(
      options.find(
        (option) => option.value === 'EXTERNAL_PLUGIN_HOST_OPERATION_DENIED'
      )?.group
    ).toBe('Plugins')
    expect(
      options.find(
        (option) => option.value === 'EXTERNAL_PLUGIN_GRANTS_CHANGED'
      )?.label
    ).toBe('Plugin Permissions Changed')
  })
  test('offers visitor enrichment so token writes are filterable', () => {
    expect(
      buildOperationOptions().find(
        (option) => option.value === 'VISITOR_ENRICHED'
      )
    ).toEqual({
      value: 'VISITOR_ENRICHED',
      label: 'Visitor Enriched',
      group: 'Analytics & Visitors',
      keywords: 'VISITOR_ENRICHED',
    })
  })
  test('offers every project group operation under Projects', () => {
    const projectOptions = buildOperationOptions().filter(
      (option) => option.group === 'Projects'
    )
    expect(projectOptions.map((option) => option.value)).toEqual([
      'TEMPLATE_INSTANTIATION_STARTED',
      'TEMPLATE_INSTANTIATION_RETRIED',
      'PROJECT_GROUP_CREATED',
      'PROJECT_GROUP_UPDATED',
      'PROJECT_GROUP_DELETED',
      'PROJECT_GROUP_PROJECT_ADDED',
      'PROJECT_GROUP_PROJECT_REMOVED',
      'PROJECT_GROUP_ENV_VAR_CREATED',
      'PROJECT_GROUP_ENV_VAR_UPDATED',
      'PROJECT_GROUP_ENV_VAR_DELETED',
      'PROJECT_GROUP_REPOSITORY_SET',
      'PROJECT_GROUP_REPOSITORY_CLEARED',
      'PROJECT_GROUP_PROJECT_FOLLOWS_REPOSITORY',
      'PROJECT_GROUP_PROJECT_UNFOLLOWS_REPOSITORY',
      'PROJECT_GROUP_ROUTE_ADDED',
      'PROJECT_GROUP_ROUTE_REMOVED',
    ])
    expect(
      projectOptions.find(
        (option) => option.value === 'PROJECT_GROUP_PROJECT_ADDED'
      )?.label
    ).toBe('Service Added to Project')
  })
  test('offers permission denials in the authentication group', () => {
    expect(
      buildOperationOptions().find(
        (option) => option.value === 'PERMISSION_DENIED'
      )
    ).toEqual({
      value: 'PERMISSION_DENIED',
      label: 'Permission Denied',
      group: 'Authentication',
      keywords: 'PERMISSION_DENIED',
    })
  })
})
