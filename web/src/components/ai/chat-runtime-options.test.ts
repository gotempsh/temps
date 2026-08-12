import { describe, expect, test } from 'bun:test'
import {
  chatProviderLabel,
  reconcileChatRuntimeAfterRefresh,
  resolveChatRuntimeSelection,
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
})

test('provider labels identify ambient host credentials', () => {
  expect(chatProviderLabel(providers[1])).toBe('Codex · Host environment')
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
