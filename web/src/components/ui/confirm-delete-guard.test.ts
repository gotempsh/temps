// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'

/**
 * Every module that can fire a destructive API call from the console must
 * route it through `ConfirmDeleteDialog`, which names the thing being removed.
 *
 * "Can fire a destructive call" is detected from the imports: a name from the
 * generated client (`@/api/client`) that starts with delete/remove/revoke/
 * destroy/purge/unlink/detach/disconnect/clear/uninstall/terminate, whether the SDK function itself
 * (`deleteFoo`) or its react-query options (`deleteFooMutation`). A module
 * that imports one must also import `ConfirmDeleteDialog`.
 *
 * Add a module to `EXEMPT` only when it genuinely cannot be reached by a click
 * without a confirmation owned elsewhere (a helper whose callers own the
 * dialog) or the action is not destructive for the user. The reason is
 * mandatory, and a stale entry fails this test.
 */
const DESTRUCTIVE_NAME =
  /^(delete|remove|revoke|destroy|purge|unlink|detach|disconnect|clear|uninstall|terminate)[A-Z]/
const DIALOG_IMPORT =
  /import\s*\{[^}]*\bConfirmDeleteDialog\b[^}]*\}\s*from\s*['"]@\/components\/ui\/confirm-delete-dialog['"]/

const EXEMPT: Record<string, string> = {
  'src/components/email/sharedUtils.ts':
    'Exports a thin deleteEmailProvider helper; its callers (EmailProvidersManagement, EmailProviderDetail) own the ConfirmDeleteDialog.',
  'src/lib/backup-cleanup.ts':
    'Exports a thin deleteBackup helper; its only caller (BackupDetail) owns the ConfirmDeleteDialog.',
  'src/lib/functions.ts':
    'Exports a thin deleteFunction helper; its only caller (ProjectFunctions) owns the ConfirmDeleteDialog.',
  'src/lib/network-policy.ts':
    'Exports a thin deleteNetworkPolicyRule helper; its only caller (NetworkPolicyPage) owns the ConfirmDeleteDialog.',
  'src/hooks/usePlugins.ts':
    'Hook module exporting useUninstallPlugin; its only caller (PluginsPage) owns the ConfirmDeleteDialog.',
  'src/components/ai/ChatSandbox.tsx':
    'Calls unlinkChatSandbox, which only detaches the sandbox from the chat; the sandbox keeps running and can be linked again from the same panel.',
  'src/components/ai/ConnectSandboxButton.tsx':
    "Calls disconnectChatSandboxProject, which only revokes the sandbox's access to a service's databases; the same button connects it again.",
  'src/pages/Drop.tsx':
    'Calls deleteProject only to roll back the service this same Drop flow just created after a failed upload; automatic cleanup, never user-triggered.',
}

const root = new URL('../../../', import.meta.url).pathname

/** Names imported from the generated client in `source`, aliases resolved to the exported name. */
export function clientImports(source: string): string[] {
  const names: string[] = []
  const importRe =
    /import\s*(?:type\s*)?\{([^}]*)\}\s*from\s*['"]@\/api\/client[^'"]*['"]/g
  for (const match of source.matchAll(importRe)) {
    for (const part of match[1].split(',')) {
      const spec = part.trim()
      if (!spec || spec.startsWith('type ')) continue
      names.push(spec.split(/\s+as\s+/)[0].trim())
    }
  }
  return names
}

export function destructiveImports(source: string): string[] {
  return clientImports(source).filter((n) => DESTRUCTIVE_NAME.test(n))
}

/** `[start, end)` of each `<ConfirmDeleteDialog ... />` opening element. */
export function dialogSpans(source: string): Array<[number, number]> {
  const spans: Array<[number, number]> = []
  let from = 0
  for (;;) {
    const start = source.indexOf('<ConfirmDeleteDialog', from)
    if (start === -1) return spans
    let depth = 0
    let i = start + '<ConfirmDeleteDialog'.length
    for (; i < source.length; i++) {
      const c = source[i]
      if (c === '{') depth++
      else if (c === '}') depth--
      else if (depth === 0 && c === '>') break
    }
    spans.push([start, i + 1])
    from = i + 1
  }
}

interface FunctionDecl {
  name: string
  start: number
  end: number
}

/**
 * Named function-like declarations (`const f = () =>`, `const f = async () =>`,
 * `function f(`) with the extent of their body, found by bracket matching.
 * Approximate by design: it only has to follow
 * `onConfirm={() => handleDelete()}` through a hop or two of handlers.
 */
function functionDecls(source: string): FunctionDecl[] {
  const re =
    /^[ \t]*(?:export\s+)?(?:(?:const|let)\s+(\w+)\s*(?::[^=\n]+)?=\s*(?:async\s*)?(?:\(|\w+\s*=>|function\b|useCallback\()|(?:async\s+)?function\s+(\w+)\s*\()/gm
  const decls: FunctionDecl[] = []
  for (const m of source.matchAll(re)) {
    const start = m.index ?? 0
    decls.push({ name: m[1] ?? m[2], start, end: bodyEnd(source, start) })
  }
  return decls
}

/** End of the statement starting at `start`: brackets balanced, then a line end. */
function bodyEnd(source: string, start: number): number {
  let depth = 0
  let seenArrowOrBody = false
  for (let i = start; i < source.length; i++) {
    const c = source[i]
    if (c === '(' || c === '{' || c === '[') {
      depth++
      seenArrowOrBody = true
    } else if (c === ')' || c === '}' || c === ']') {
      depth--
      if (depth < 0) return i
    } else if (depth === 0 && (c === ';' || c === '\n') && seenArrowOrBody) {
      const before = source.slice(start, i).trimEnd()
      if (c === ';' || !/(=>|=|\(|,|&&|\|\||\?|:)$/.test(before)) return i
    }
  }
  return source.length
}

const IDENT = /[A-Za-z_$][\w$]*/g

/** Call sites of destructive operations that no ConfirmDeleteDialog reaches. */
export function unconfirmedCalls(source: string): string[] {
  const destructive = destructiveImports(source)
  if (destructive.length === 0) return []

  const decls = functionDecls(source)
  const spans = dialogSpans(source)
  const reachable = new Set<string>()
  for (const [start, end] of spans) {
    for (const id of source.slice(start, end).match(IDENT) ?? []) {
      reachable.add(id)
    }
  }
  // Follow handlers called from handlers until nothing new is reachable.
  for (let changed = true; changed;) {
    changed = false
    for (const decl of decls) {
      if (!reachable.has(decl.name)) continue
      for (const id of source.slice(decl.start, decl.end).match(IDENT) ?? []) {
        if (!reachable.has(id)) {
          reachable.add(id)
          changed = true
        }
      }
    }
  }

  const inDialog = (index: number) =>
    spans.some(([start, end]) => index >= start && index < end)
  const confirmed = (index: number) => {
    if (inDialog(index)) return true
    // Any enclosing function counts: a call nested in an inline callback is
    // reached through whichever outer handler the dialog invokes.
    return decls
      .filter((d) => d.start <= index && index < d.end)
      .some((d) => reachable.has(d.name))
  }

  // Every `const v = useMutation({...})`, with the extent of its options.
  const mutations: Array<{
    local: string
    start: number
    end: number
    via: string | null
  }> = []
  for (const m of source.matchAll(
    /(?:const|let)\s+(\w+)\s*=\s*useMutation\(/g
  )) {
    const start = (m.index ?? 0) + m[0].length - 1
    let depth = 0
    let end = start
    for (; end < source.length; end++) {
      if (source[end] === '(') depth++
      else if (source[end] === ')' && --depth === 0) break
    }
    const body = source.slice(start, end)
    const used = destructive.find((n) =>
      new RegExp(`\\b${n}\\s*\\(`).test(body)
    )
    mutations.push({ local: m[1], start, end, via: used ?? null })
  }
  const inMutationBody = (index: number) =>
    mutations.some((mu) => mu.via && index >= mu.start && index < mu.end)

  const ignored = (index: number) => {
    const lineStart = source.lastIndexOf('\n', index - 1)
    const context = source.slice(
      Math.max(0, source.lastIndexOf('\n', lineStart - 1) - 200),
      index
    )
    return /confirm-delete-guard: ignore/.test(
      context.split('\n').slice(-4).join('\n')
    )
  }

  // What must only be reached through the dialog: SDK functions, mutation
  // variables built from them, and local helpers that wrap an SDK function.
  type Callable = { local: string; via: string; kind: 'fn' | 'mutate' }
  const queue: Callable[] = []
  const seen = new Set<string>()
  const add = (c: Callable) => {
    if (seen.has(c.local)) return
    seen.add(c.local)
    queue.push(c)
  }
  for (const name of destructive) {
    if (!name.endsWith('Mutation')) add({ local: name, via: name, kind: 'fn' })
  }
  for (const mu of mutations) {
    if (mu.via) add({ local: mu.local, via: mu.via, kind: 'mutate' })
  }

  const problems: string[] = []
  while (queue.length > 0) {
    const { local, via, kind } = queue.shift() as Callable
    const call =
      kind === 'fn'
        ? new RegExp(`(?<![\\w.])${local}\\s*\\(`, 'g')
        : new RegExp(`\\b${local}\\.(?:mutate|mutateAsync)\\s*\\(`, 'g')
    const own = mutations.find((mu) => mu.local === local)
    for (const m of source.matchAll(call)) {
      const index = m.index ?? 0
      // Declaration of the helper itself, or a retry of an already confirmed
      // request after a step-up verification, from inside its own options.
      if (
        kind === 'fn' &&
        /(?:function|const|let)\s+$/.test(source.slice(0, index))
      ) {
        continue
      }
      if (own && index >= own.start && index < own.end) continue
      // SDK calls inside a useMutation are checked at the `.mutate` sites.
      if (kind === 'fn' && inMutationBody(index)) continue
      if (ignored(index) || confirmed(index)) continue
      const fn = decls
        .filter((d) => d.start <= index && index < d.end)
        .sort((x, y) => y.start - x.start)[0]
      // A lower-case helper that merely wraps the SDK call: its own callers
      // are what must be confirmed.
      if (kind === 'fn' && fn && /^[a-z]/.test(fn.name) && fn.name !== local) {
        // Reached only through a mutation built around the helper: that
        // mutation's `.mutate` sites are what must be confirmed.
        const wrapped = mutations.filter((mu) =>
          new RegExp(`\\b${fn.name}\\s*\\(`).test(
            source.slice(mu.start, mu.end)
          )
        )
        if (wrapped.length > 0) {
          for (const mu of wrapped) {
            mu.via ??= via
            add({ local: mu.local, via, kind: 'mutate' })
          }
          continue
        }
      }
      const line = source.slice(0, index).split('\n').length
      problems.push(
        `${local} (${via}) at line ${line} in ${fn?.name ?? '<module>'}`
      )
    }
  }
  return problems
}

async function sourceFiles(): Promise<string[]> {
  const files: string[] = []
  for await (const path of new Bun.Glob('src/**/*.{ts,tsx}').scan(root)) {
    if (path.startsWith('src/api/client/')) continue
    if (path.startsWith('src/test/')) continue
    if (/\.test\.tsx?$/.test(path)) continue
    files.push(path)
  }
  return files.sort()
}

test('modules that import a destructive API call also import ConfirmDeleteDialog', async () => {
  const violations: string[] = []
  for (const path of await sourceFiles()) {
    if (path in EXEMPT) continue
    const source = await Bun.file(`${root}${path}`).text()
    const destructive = destructiveImports(source)
    if (destructive.length === 0) continue
    if (!DIALOG_IMPORT.test(source)) {
      violations.push(`${path}: ${destructive.join(', ')}`)
    }
  }
  expect(violations).toEqual([])
}, 30_000)

test('every destructive call is reachable only through a ConfirmDeleteDialog', async () => {
  const violations: string[] = []
  for (const path of await sourceFiles()) {
    if (path in EXEMPT) continue
    const source = await Bun.file(`${root}${path}`).text()
    for (const problem of unconfirmedCalls(source)) {
      violations.push(`${path}: ${problem}`)
    }
  }
  expect(violations).toEqual([])
}, 30_000)

test('every exemption is justified and still needed', async () => {
  for (const [path, reason] of Object.entries(EXEMPT)) {
    expect(reason.trim().length).toBeGreaterThan(20)
    const file = Bun.file(`${root}${path}`)
    expect(await file.exists()).toBe(true)
    const source = await file.text()
    expect(destructiveImports(source).length).toBeGreaterThan(0)
    expect(DIALOG_IMPORT.test(source)).toBe(false)
  }
}, 30_000)

test('the detector recognises SDK functions, mutation options and aliases', () => {
  const source = `
    import {
      deleteEmailProvider as deleteApi,
      listEmailProviders,
      removeTeamMemberMutation,
      type DeleteThing,
    } from '@/api/client'
    import { revokeApiKeyMutation } from '@/api/client/@tanstack/react-query.gen'
  `
  expect(destructiveImports(source)).toEqual([
    'deleteEmailProvider',
    'removeTeamMemberMutation',
    'revokeApiKeyMutation',
  ])
})

const FIXTURE_HEADER = `
  import { deleteThingMutation, deleteOtherMutation, removeRow } from '@/api/client'
  import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
`

test('a call reached from onConfirm, directly or through a handler, is confirmed', () => {
  const source = `${FIXTURE_HEADER}
    function Page() {
      const del = useMutation({ ...deleteThingMutation() })
      const doDelete = () => del.mutate({ path: { id: 1 } })
      const handleConfirm = async () => {
        await doDelete()
      }
      return (
        <ConfirmDeleteDialog
          open
          onOpenChange={() => {}}
          resourceType="thing"
          resourceName="x"
          onConfirm={() => void handleConfirm()}
        />
      )
    }
  `
  expect(unconfirmedCalls(source)).toEqual([])
})

test('importing the dialog does not excuse a second, direct destructive call', () => {
  const source = `${FIXTURE_HEADER}
    function Page() {
      const del = useMutation({ ...deleteThingMutation() })
      const other = useMutation({ ...deleteOtherMutation() })
      const handleConfirm = () => del.mutate({ path: { id: 1 } })
      return (
        <>
          <button onClick={() => other.mutate({ path: { id: 2 } })}>x</button>
          <ConfirmDeleteDialog
            open
            onOpenChange={() => {}}
            resourceType="thing"
            resourceName="x"
            onConfirm={handleConfirm}
          />
        </>
      )
    }
  `
  const problems = unconfirmedCalls(source)
  expect(problems.length).toBe(1)
  expect(problems[0]).toContain('other')
})

test('a direct SDK call outside any confirmed handler is flagged', () => {
  const source = `${FIXTURE_HEADER}
    function Page() {
      const onClick = async () => {
        await removeRow({ path: { id: 3 } })
      }
      return <button onClick={onClick}>x</button>
    }
  `
  expect(unconfirmedCalls(source).length).toBe(1)
})
