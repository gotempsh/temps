// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { GitChangeKind } from '@/api/client'

const LANGUAGE_BY_EXTENSION: Record<string, string> = {
  ts: 'typescript',
  tsx: 'typescript',
  mts: 'typescript',
  cts: 'typescript',
  js: 'javascript',
  jsx: 'javascript',
  mjs: 'javascript',
  cjs: 'javascript',
  json: 'json',
  css: 'css',
  scss: 'scss',
  less: 'less',
  html: 'html',
  htm: 'html',
  vue: 'html',
  svelte: 'html',
  md: 'markdown',
  mdx: 'markdown',
  py: 'python',
  rs: 'rust',
  go: 'go',
  rb: 'ruby',
  php: 'php',
  java: 'java',
  kt: 'kotlin',
  swift: 'swift',
  c: 'c',
  h: 'c',
  cpp: 'cpp',
  cc: 'cpp',
  hpp: 'cpp',
  cs: 'csharp',
  yaml: 'yaml',
  yml: 'yaml',
  toml: 'ini',
  ini: 'ini',
  sh: 'shell',
  bash: 'shell',
  zsh: 'shell',
  sql: 'sql',
  xml: 'xml',
  svg: 'xml',
  graphql: 'graphql',
  gql: 'graphql',
}

/** File name -> last path segment, e.g. `/a/b/c.ts` -> `c.ts`. */
export function baseName(path: string): string {
  const trimmed = path.replace(/\/+$/, '')
  const slash = trimmed.lastIndexOf('/')
  return slash === -1 ? trimmed : trimmed.slice(slash + 1)
}

/** The Monaco language that highlights `path`, `plaintext` when unknown. */
export function languageForPath(path: string): string {
  const name = baseName(path).toLowerCase()
  if (name === 'dockerfile' || name.startsWith('dockerfile.')) {
    return 'dockerfile'
  }
  if (name === 'makefile') return 'makefile'
  const dot = name.lastIndexOf('.')
  if (dot <= 0) return 'plaintext'
  return LANGUAGE_BY_EXTENSION[name.slice(dot + 1)] ?? 'plaintext'
}

/**
 * `path` relative to `root`, for display: the sandbox lists absolute paths,
 * which repeat the work directory on every line.
 */
export function relativeTo(root: string, path: string): string {
  const base = root.replace(/\/+$/, '')
  if (path === base) return '.'
  return path.startsWith(`${base}/`) ? path.slice(base.length + 1) : path
}

/** How a changed file is shown: a one-letter badge, its meaning, a colour. */
export interface ChangeBadge {
  letter: string
  label: string
  className: string
}

const CHANGE_BADGES: Record<GitChangeKind, ChangeBadge> = {
  modified: {
    letter: 'M',
    label: 'Modified',
    className: 'text-amber-600 dark:text-amber-400',
  },
  added: {
    letter: 'A',
    label: 'Added',
    className: 'text-emerald-600 dark:text-emerald-400',
  },
  untracked: {
    letter: 'U',
    label: 'New file',
    className: 'text-emerald-600 dark:text-emerald-400',
  },
  deleted: {
    letter: 'D',
    label: 'Deleted',
    className: 'text-red-600 dark:text-red-400',
  },
  renamed: {
    letter: 'R',
    label: 'Renamed',
    className: 'text-sky-600 dark:text-sky-400',
  },
  copied: {
    letter: 'C',
    label: 'Copied',
    className: 'text-sky-600 dark:text-sky-400',
  },
  type_changed: {
    letter: 'T',
    label: 'Type changed',
    className: 'text-amber-600 dark:text-amber-400',
  },
  conflicted: {
    letter: '!',
    label: 'Conflicted',
    className: 'text-red-600 dark:text-red-400',
  },
}

export function changeBadge(kind: GitChangeKind): ChangeBadge {
  return CHANGE_BADGES[kind]
}

/** Total changed files across repositories, for the tab's count. */
export function countChanges(repos: ReadonlyArray<{ files: unknown[] }>) {
  return repos.reduce((total, repo) => total + repo.files.length, 0)
}

/** `bytes` for people: `512 B`, `3.4 KB`, `1.2 MB`. */
export function formatFileSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`
}
