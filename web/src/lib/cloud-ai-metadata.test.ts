// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  CLOUD_AI_METADATA_KEYS,
  cloudAiMetadataMissing,
  withCloudAiMetadata,
} from './cloud-ai-metadata'

describe('Cloud AI metadata consent', () => {
  test('identifies missing Cloud provider metadata without hiding local traces', () => {
    const settings = {
      write_mode: 'cloud',
      fidelity: 'queryable',
      attribute_allowlist: [] as string[],
    }
    expect(cloudAiMetadataMissing(settings)).toBe(true)
    expect(cloudAiMetadataMissing({ ...settings, write_mode: 'local' })).toBe(
      false
    )
    expect(
      cloudAiMetadataMissing({
        ...settings,
        attribute_allowlist: [...CLOUD_AI_METADATA_KEYS],
      })
    ).toBe(false)
    for (const key of ['gen_ai.system', 'gen_ai.provider.name']) {
      expect(
        cloudAiMetadataMissing({ ...settings, attribute_allowlist: [key] })
      ).toBe(true)
    }
    expect(
      cloudAiMetadataMissing({
        ...settings,
        attribute_allowlist: ['gen_ai.request.model'],
      })
    ).toBe(true)
  })

  test('enabling adds only exact metadata keys and preserves existing consent', () => {
    const original = ['custom.safe', 'gen_ai.system']
    const enabled = withCloudAiMetadata(original, true)
    expect(enabled).toContain('custom.safe')
    expect(new Set(enabled).size).toBe(enabled.length)
    expect(enabled.filter((key) => key !== 'custom.safe')).toHaveLength(
      CLOUD_AI_METADATA_KEYS.length
    )
    for (const excluded of [
      'gen_ai.input.messages',
      'gen_ai.output.messages',
      'gen_ai.system_instructions',
      'gen_ai.tool.call.arguments',
      '*',
      'gen_ai.*',
    ]) {
      expect(enabled).not.toContain(excluded)
    }
    expect(original).toEqual(['custom.safe', 'gen_ai.system'])
  })

  test('disabling removes metadata keys without changing separately consented attributes', () => {
    expect(
      withCloudAiMetadata(
        ['custom.safe', 'gen_ai.input.messages', ...CLOUD_AI_METADATA_KEYS],
        false
      )
    ).toEqual(['custom.safe', 'gen_ai.input.messages'])
  })
})
