// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { readZip } from '@/test/read-zip'
import {
  buildClaudeExtension,
  claudeExtensionKeyName,
  claudeExtensionManifest,
  extensionCanConnect,
} from './extension'

describe('the Claude app extension', () => {
  test('the bundle holds the manifest and the server, and no credential', () => {
    const files = readZip(
      buildClaudeExtension('http://localhost:3014/setup?x=1', {
        bridge: 'BRIDGE',
        deploy: 'DEPLOY',
      })
    )
    expect([...files.keys()]).toEqual([
      'manifest.json',
      'server/index.js',
      'server/bridge.cjs',
      'server/deploy.cjs',
    ])
    expect(files.get('server/bridge.cjs')).toBe('BRIDGE')
    expect(files.get('server/deploy.cjs')).toBe('DEPLOY')
    // The entry starts the bridge whoever loads it.
    expect(files.get('server/index.js')).toContain(
      "require('./bridge.cjs').main()"
    )
    expect(files.get('server/index.js')).not.toContain('require.main')
    const manifest = JSON.parse(files.get('manifest.json') ?? '')
    expect(manifest.user_config.server_url.default).toBe(
      'http://localhost:3014'
    )
    expect(files.get('manifest.json')).not.toMatch(/tk_|Bearer/)
  })

  test('the key is a sensitive setting the user enters on install', () => {
    const manifest = claudeExtensionManifest('https://temps.example.com')
    expect(manifest.manifest_version).toBe('0.3')
    expect(manifest.user_config.api_key).toMatchObject({
      type: 'string',
      sensitive: true,
      required: true,
    })
    expect('default' in manifest.user_config.api_key).toBe(false)
    expect(manifest.server.mcp_config).toEqual({
      command: 'node',
      args: ['${__dirname}/server/index.js', '${user_config.deploy_folders}'],
      env: {
        TEMPS_URL: '${user_config.server_url}',
        TEMPS_API_KEY: '${user_config.api_key}',
      },
    })
  })

  test('it lists the write tools; the key decides what a write may do', () => {
    const manifest = claudeExtensionManifest('https://temps.example.com')
    expect(manifest.tools.map((tool) => tool.name)).toEqual([
      'temps',
      'temps_write',
      'confirm_action',
      'temps_deploy_folder',
    ])
    expect(manifest.long_description).toContain('approve')
  })

  test('deploying from this computer is limited to folders the user picks, none by default', () => {
    const manifest = claudeExtensionManifest('https://temps.example.com')
    expect(manifest.user_config.deploy_folders).toEqual({
      type: 'directory',
      title: 'Folders Claude may deploy',
      description: expect.stringContaining('Leave empty'),
      multiple: true,
      required: false,
    })
    expect('default' in manifest.user_config.deploy_folders).toBe(false)
    expect(manifest.long_description).toContain('.gitignore')
  })

  test('its key name is an MCP client key and unique to the second', () => {
    expect(claudeExtensionKeyName(new Date('2026-11-25T10:31:05.123Z'))).toBe(
      'mcp-claude-app-2026-11-25T10-31-05Z'
    )
  })

  test('it connects over HTTPS, or plain HTTP only to this computer or a tailnet', () => {
    for (const origin of [
      'https://temps.example.com',
      'https://192.168.1.10',
      'http://localhost:3014',
      'http://127.0.0.1:8080',
      'http://[::1]:3000',
      'http://100.101.102.103',
      'http://server.tailnet-1234.ts.net',
      'http://nas:3000',
    ]) {
      expect(extensionCanConnect(origin)).toBe(true)
    }
    for (const origin of [
      'http://192.168.1.10:8080',
      'http://10.0.0.4',
      'http://temps.example.com',
      'ftp://temps.example.com',
      'not a url',
    ]) {
      expect(extensionCanConnect(origin)).toBe(false)
    }
  })
})
