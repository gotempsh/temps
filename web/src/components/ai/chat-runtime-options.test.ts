// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  chatHarnessProviderOptions,
  chatModelLabel,
  chatPermissionLabel,
  chatProviderLabel,
  chatThinkingItemContent,
  chatThinkingLabel,
  reconcileChatRuntimeAfterRefresh,
  resolveChatRuntimeSelection,
  usesHarnessCatalog,
  type ChatProviderOption,
} from './chat-runtime-options'

const providers: ChatProviderOption[] = [
  {
    id: 'gateway_key:7',
    name: 'OpenAI Production',
    auth_source: 'configured_key',
    models: [
      {
        id: 'gpt-5.4',
        name: 'gpt-5.4',
        thinking_options: [
          { id: 'low', name: 'Low' },
          { id: 'medium', name: 'Medium' },
        ],
        default_thinking_option_id: 'medium',
      },
    ],
    default_model_id: 'gpt-5.4',
    permission_modes: [{ id: 'confirm-actions', name: 'Confirm actions' }],
    default_permission_mode_id: 'confirm-actions',
  },
  {
    id: 'codex_cli',
    name: 'Codex',
    auth_source: 'host_environment',
    models: [
      {
        id: 'gpt-5.4-codex',
        name: 'gpt-5.4-codex',
        thinking_options: [{ id: 'high', name: 'High' }],
      },
    ],
    permission_modes: [
      { id: 'auto', name: 'Default permissions' },
      { id: 'full-access', name: 'Full access' },
    ],
    default_permission_mode_id: 'auto',
  },
]

describe('usesHarnessCatalog', () => {
  test('uses host harness capabilities for application and global threads', () => {
    expect(usesHarnessCatalog('application')).toBe(true)
    expect(usesHarnessCatalog('global')).toBe(true)
    expect(usesHarnessCatalog('deployment')).toBe(false)
  })
})

describe('resolveChatRuntimeSelection', () => {
  test('uses provider and model defaults for a new chat', () => {
    expect(resolveChatRuntimeSelection(providers, 'gateway_key:7')).toEqual({
      providerId: 'gateway_key:7',
      modelId: 'gpt-5.4',
      thinkingOptionId: 'medium',
      permissionModeId: 'confirm-actions',
    })
  })

  test('drops values that belong to the previous provider', () => {
    expect(
      resolveChatRuntimeSelection(providers, 'codex_cli', {
        modelId: 'gpt-5.4',
        thinkingOptionId: 'medium',
        permissionModeId: 'confirm-actions',
      })
    ).toEqual({
      providerId: 'codex_cli',
      modelId: 'gpt-5.4-codex',
      thinkingOptionId: 'high',
      permissionModeId: 'auto',
    })
  })

  test('does not invent a thinking level for a non-reasoning model', () => {
    const withoutThinking: ChatProviderOption[] = [
      {
        ...providers[0],
        models: [
          {
            id: 'gpt-4.1-mini',
            name: 'gpt-4.1-mini',
            thinking_options: [],
          },
        ],
        default_model_id: 'gpt-4.1-mini',
      },
    ]

    expect(
      resolveChatRuntimeSelection(withoutThinking, 'gateway_key:7')
        .thinkingOptionId
    ).toBeNull()
  })

  test('keeps reasoning for Responses API models with project tools', () => {
    const responsesModel: ChatProviderOption[] = [
      {
        ...providers[0],
        models: [
          {
            id: 'gpt-5.6-luna',
            name: 'gpt-5.6-luna',
            thinking_options: [
              { id: 'none', name: 'None' },
              { id: 'medium', name: 'Medium' },
            ],
            default_thinking_option_id: 'medium',
          },
        ],
        default_model_id: 'gpt-5.6-luna',
      },
    ]

    expect(
      resolveChatRuntimeSelection(responsesModel, 'gateway_key:7', {
        thinkingOptionId: 'medium',
      }).thinkingOptionId
    ).toBe('medium')
  })
})

test('provider labels identify ambient host credentials', () => {
  expect(chatProviderLabel(providers[1])).toBe('Codex · Host environment')
})

test('runtime labels expose behavior instead of provider sentinels', () => {
  const claude: ChatProviderOption = {
    id: 'claude_cli',
    name: 'Claude Code',
    auth_source: 'host_environment',
    models: [],
    permission_modes: [],
  }

  expect(
    chatModelLabel(claude, {
      id: 'default',
      name: 'Default · Opus 5',
      thinking_options: [],
    })
  ).toBe('Opus 5')
  expect(
    chatModelLabel(claude, {
      id: 'default',
      name: 'default',
      thinking_options: [],
    })
  ).toBe('Claude Code model')
  expect(chatThinkingLabel({ id: 'default', name: 'Default' })).toBe('Auto')
  expect(typeof chatThinkingItemContent({ id: 'medium', name: 'Medium' })).toBe(
    'string'
  )
  expect(chatThinkingItemContent({ id: 'medium', name: 'Medium' })).toBe(
    'Medium'
  )
  expect(chatPermissionLabel({ id: 'default', name: 'Default' })).toBe(
    'Ask each time'
  )
  expect(chatPermissionLabel({ id: 'auto', name: 'Default permissions' })).toBe(
    'Auto'
  )
})

test('harness catalog options preserve resolved runtime controls', () => {
  expect(
    chatHarnessProviderOptions([
      {
        id: 'claude_cli',
        name: 'Claude Code',
        workspace_ready: true,
        runtime_models: [
          {
            id: 'default',
            name: 'Opus 5',
            thinking_modes: [{ id: 'high', name: 'High' }],
            default_thinking_mode_id: 'high',
          },
        ],
        default_runtime_model_id: 'default',
        permission_modes: [{ id: 'full-access', name: 'Auto' }],
        default_permission_mode_id: 'full-access',
      },
    ])
  ).toEqual([
    {
      id: 'claude_cli',
      name: 'Claude Code',
      auth_source: 'host_environment',
      models: [
        {
          id: 'default',
          name: 'Opus 5',
          thinking_options: [{ id: 'high', name: 'High' }],
          tool_thinking_options: undefined,
          default_thinking_option_id: 'high',
        },
      ],
      default_model_id: 'default',
      model_discovery_status: 'ready',
      model_discovery_error: null,
      permission_modes: [{ id: 'full-access', name: 'Auto' }],
      default_permission_mode_id: 'full-access',
    },
  ])
})

test('harness catalog excludes a host-only CLI without a workspace relay', () => {
  expect(
    chatHarnessProviderOptions([
      {
        id: 'codex_cli',
        name: 'Codex',
        workspace_ready: false,
        runtime_models: [],
        default_runtime_model_id: null,
        permission_modes: [],
        default_permission_mode_id: 'default',
      },
    ])
  ).toEqual([])
})

test('a model refresh drops a stale thinking sentinel without switching harnesses', () => {
  const refreshed: ChatProviderOption[] = [
    {
      id: 'opencode',
      name: 'OpenCode',
      auth_source: 'host_environment',
      models: [
        {
          id: 'opencode/big-pickle',
          name: 'Big Pickle',
          thinking_options: [],
        },
      ],
      default_model_id: 'opencode/big-pickle',
      permission_modes: [{ id: 'build', name: 'Build' }],
      default_permission_mode_id: 'build',
    },
  ]

  expect(
    reconcileChatRuntimeAfterRefresh(refreshed, {
      providerId: 'opencode',
      modelId: 'opencode/big-pickle',
      thinkingOptionId: 'default',
      permissionModeId: 'build',
    })
  ).toEqual({
    providerId: 'opencode',
    modelId: 'opencode/big-pickle',
    thinkingOptionId: null,
    permissionModeId: 'build',
  })
})
