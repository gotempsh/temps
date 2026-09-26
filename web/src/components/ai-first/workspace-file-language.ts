// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { CodeLanguage } from '@/components/ui/code-block'

export function workspaceFileLanguage(path: string): CodeLanguage {
  const name = path.split('/').pop()?.toLowerCase() ?? ''
  if (name === 'dockerfile' || name.startsWith('dockerfile.'))
    return 'dockerfile'
  const extension = name.includes('.') ? name.split('.').pop() : ''
  switch (extension) {
    case 'sh':
    case 'bash':
    case 'zsh':
      return 'bash'
    case 'yaml':
    case 'yml':
      return 'yaml'
    case 'json':
    case 'jsonc':
      return 'json'
    case 'js':
    case 'mjs':
    case 'cjs':
      return 'javascript'
    case 'ts':
    case 'mts':
    case 'cts':
      return 'typescript'
    case 'jsx':
    case 'tsx':
      return 'tsx'
    case 'py':
      return 'python'
    case 'go':
      return 'go'
    case 'rs':
      return 'rust'
    case 'rb':
      return 'ruby'
    case 'php':
      return 'php'
    case 'java':
      return 'java'
    case 'sql':
      return 'sql'
    case 'css':
    case 'scss':
      return 'css'
    case 'html':
    case 'htm':
      return 'html'
    case 'md':
    case 'mdx':
      return 'markdown'
    case 'toml':
      return 'toml'
    case 'xml':
    case 'svg':
      return 'xml'
    default:
      return 'text'
  }
}
