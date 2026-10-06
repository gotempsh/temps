// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import { matchRoutes, type RouteObject } from 'react-router'
import ts from 'typescript'

import { autopilotPaths } from '@/components/agents/AutopilotPage'
import {
  projectNavItems,
  projectNavItemsFor,
} from '@/components/command/CommandPalette'
import {
  isExternalProjectPage,
  PROJECT_SECTION_LINKS,
} from '@/lib/project-navigation'
import { LEGACY_PROJECT_ROUTES } from '@/lib/project-settings-sections'

/**
 * The project route tables live inline in JSX, so read them from the source
 * the browser actually runs instead of a hand-maintained copy that could
 * drift. A `<Route>` whose path this reader cannot resolve fails the test
 * rather than being skipped.
 */
type ParsedRoute = RouteObject & { elementName?: string }

const SRC = join(import.meta.dir, '..')

/** Splat routes whose element owns a nested `<Routes>` table. */
const NESTED_ROUTE_TABLES: Record<string, string> = {
  ProjectSettings: 'components/project/ProjectSettings.tsx',
}

function attribute(node: ts.JsxOpeningLikeElement, name: string) {
  return node.attributes.properties.find(
    (prop): prop is ts.JsxAttribute =>
      ts.isJsxAttribute(prop) && prop.name.getText() === name
  )
}

function elementName(node: ts.JsxOpeningLikeElement): string | undefined {
  const element = attribute(node, 'element')?.initializer
  if (!element || !ts.isJsxExpression(element) || !element.expression) return
  let expression: ts.Expression = element.expression
  while (ts.isParenthesizedExpression(expression))
    expression = expression.expression
  if (ts.isJsxSelfClosingElement(expression))
    return expression.tagName.getText()
  if (ts.isJsxElement(expression))
    return expression.openingElement.tagName.getText()
}

function unwrap(expression: ts.Expression): ts.Expression {
  while (
    ts.isParenthesizedExpression(expression) ||
    ts.isAsExpression(expression)
  )
    expression = expression.expression
  return expression
}

function openingOf(node: ts.Node): ts.JsxOpeningLikeElement | undefined {
  if (ts.isJsxSelfClosingElement(node)) return node
  if (ts.isJsxElement(node)) return node.openingElement
}

function routeChildren(node: ts.Node): ts.Node[] {
  return ts.isJsxElement(node) ? [...node.children] : []
}

/** Builds route objects from `<Route>` children, including `[...].map` lists. */
function parseRoutes(
  children: readonly ts.Node[],
  file: string,
  mapped?: { param: string; values: string[] }
): ParsedRoute[] {
  const routes: ParsedRoute[] = []
  for (const child of children) {
    if (ts.isJsxExpression(child) && child.expression) {
      const call = unwrap(child.expression)
      if (
        !ts.isCallExpression(call) ||
        !ts.isPropertyAccessExpression(call.expression) ||
        call.expression.name.text !== 'map'
      )
        continue
      const list = unwrap(call.expression.expression)
      const callback = call.arguments[0]
      if (
        !ts.isArrayLiteralExpression(list) ||
        !callback ||
        !ts.isArrowFunction(callback)
      )
        throw new Error(`${file}: unsupported route list ${call.getText()}`)
      const values = list.elements.map((item) => {
        if (!ts.isStringLiteral(item))
          throw new Error(`${file}: non-literal route path ${item.getText()}`)
        return item.text
      })
      const body = unwrap(callback.body as ts.Expression)
      routes.push(
        ...parseRoutes([body], file, {
          param: callback.parameters[0].name.getText(),
          values,
        })
      )
      continue
    }
    const opening = openingOf(child)
    if (!opening || opening.tagName.getText() !== 'Route') continue
    const path = attribute(opening, 'path')?.initializer
    const nested = parseRoutes(routeChildren(child), file)
    const base = {
      elementName: elementName(opening),
      ...(nested.length ? { children: nested } : {}),
    }
    if (attribute(opening, 'index')) {
      routes.push({ ...base, index: true } as ParsedRoute)
    } else if (path && ts.isStringLiteral(path)) {
      routes.push({ ...base, path: path.text })
    } else if (
      mapped &&
      path &&
      ts.isJsxExpression(path) &&
      path.expression?.getText() === mapped.param
    ) {
      for (const value of mapped.values) routes.push({ ...base, path: value })
    } else {
      throw new Error(`${file}: cannot resolve ${opening.getText()}`)
    }
  }
  return routes
}

function isExternalBranch(node: ts.Node): boolean {
  for (let parent = node.parent; parent; parent = parent.parent)
    if (
      ts.isIfStatement(parent) &&
      parent.expression.getText().includes("source_type === 'external'")
    )
      return true
  return false
}

/** Every top-level `<Routes>` table in a file, keyed by project kind. */
function readRouteTables(relativePath: string) {
  const file = join(SRC, relativePath)
  const source = ts.createSourceFile(
    file,
    readFileSync(file, 'utf8'),
    ts.ScriptTarget.Latest,
    true,
    ts.ScriptKind.TSX
  )
  const tables: { external: ParsedRoute[][]; hosted: ParsedRoute[][] } = {
    external: [],
    hosted: [],
  }
  const visit = (node: ts.Node) => {
    if (
      ts.isJsxElement(node) &&
      node.openingElement.tagName.getText() === 'Routes'
    ) {
      tables[isExternalBranch(node) ? 'external' : 'hosted'].push(
        parseRoutes(node.children, relativePath)
      )
      return
    }
    ts.forEachChild(node, visit)
  }
  visit(source)
  return tables
}

const projectDetail = readRouteTables('pages/ProjectDetail.tsx')
const PROJECT_ROUTES = {
  external: projectDetail.external[0],
  hosted: projectDetail.hosted[0],
}

/**
 * Describes where a project-relative URL lands, or why it does not land on a
 * real page: no match, the `*` fallback, or a static segment swallowed by a
 * `:param` (how `errors/alert-rules` used to render an error group).
 */
function resolveUrl(routes: ParsedRoute[], url: string): string {
  const pathname = url.split('?')[0]
  const matches = matchRoutes(routes, `/${pathname}`)
  if (!matches) return `${pathname}: no route`
  const leaf = matches[matches.length - 1]
  const route = leaf.route as ParsedRoute
  if (route.path === '*') return `${pathname}: catch-all fallback`
  const captured = Object.keys(leaf.params).filter((key) => key !== '*')
  if (captured.length)
    return `${pathname}: static URL captured by :${captured.join(', :')}`
  const nestedFile = route.elementName && NESTED_ROUTE_TABLES[route.elementName]
  if (nestedFile && route.path?.endsWith('/*')) {
    const nested = readRouteTables(nestedFile).hosted[0]
    const rest = resolveUrl(nested, leaf.params['*'] ?? '')
    return rest.endsWith('ok') ? 'ok' : `${pathname} -> ${rest}`
  }
  return 'ok'
}

/** Elements that only forward to another URL. */
const REDIRECT_ELEMENTS = new Set([
  'Navigate',
  'LegacyProjectRouteRedirect',
  'RenamedProjectRouteRedirect',
])

/**
 * The innermost route a URL lands on, following splat routes into the nested
 * route tables this suite knows about, with the full pattern that matched.
 */
function resolveLeaf(
  routes: ParsedRoute[],
  url: string
): { route: ParsedRoute; pattern: string } | undefined {
  const pathname = url.split('?')[0]
  const matches = matchRoutes(routes, `/${pathname}`)
  if (!matches) return undefined
  const leaf = matches[matches.length - 1]
  const route = leaf.route as ParsedRoute
  const pattern = matches
    .map((match) => (match.route as ParsedRoute).path)
    .filter(Boolean)
    .join('/')
  const nestedFile = route.elementName && NESTED_ROUTE_TABLES[route.elementName]
  if (nestedFile && route.path?.endsWith('/*')) {
    const nested = resolveLeaf(
      readRouteTables(nestedFile).hosted[0],
      leaf.params['*'] ?? ''
    )
    return (
      nested && {
        route: nested.route,
        pattern: `${pattern.replace(/\/\*$/, '')}/${nested.pattern}`,
      }
    )
  }
  return { route, pattern }
}

/** Paths registered more than once at the same level of a route table. */
function duplicatePaths(routes: ParsedRoute[], prefix = ''): string[] {
  const seen = new Set<string>()
  const duplicates: string[] = []
  for (const route of routes) {
    if (route.children)
      duplicates.push(
        ...duplicatePaths(route.children as ParsedRoute[], `${route.path}/`)
      )
    if (!route.path) continue
    const path = `${prefix}${route.path}`
    if (seen.has(path)) duplicates.push(path)
    seen.add(path)
  }
  return duplicates
}

/** The section-nav links ProjectSectionLayout renders for an external project. */
function externalSectionUrls(): string[] {
  const hidden = ['analytics/api-traffic', 'ai-crawlers']
  return (['analytics', 'errors', 'traces', 'monitoring'] as const)
    .flatMap((section) => PROJECT_SECTION_LINKS[section] ?? [])
    .map((link) => link.url)
    .filter((url) => !hidden.includes(url))
    .concat(['settings/general', 'settings/telemetry'])
}

describe('project route tables', () => {
  test('are read from ProjectDetail for both project kinds', () => {
    expect(projectDetail.external).toHaveLength(1)
    expect(projectDetail.hosted).toHaveLength(1)
    expect(PROJECT_ROUTES.external.length).toBeGreaterThan(10)
    expect(PROJECT_ROUTES.hosted.length).toBeGreaterThan(10)
  })

  test('flags the failure modes this suite guards against', () => {
    const routes: ParsedRoute[] = [
      { path: 'errors/:errorGroupId' },
      { path: '*' },
    ]
    expect(resolveUrl(routes, 'errors/alert-rules')).toContain(':errorGroupId')
    expect(resolveUrl(routes, 'revenue')).toContain('catch-all')
    expect(resolveUrl([{ path: 'project' }], 'workspace')).toContain('no route')
    expect(resolveUrl(PROJECT_ROUTES.hosted, 'settings/nope')).toContain(
      'catch-all'
    )
  })
})

describe('section navigation', () => {
  test('every hosted section link opens a registered page', () => {
    const urls = Object.values(PROJECT_SECTION_LINKS).flatMap((links) =>
      links.map((link) => link.url)
    )
    expect(
      urls
        .map((url) => resolveUrl(PROJECT_ROUTES.hosted, url))
        .filter((r) => r !== 'ok')
    ).toEqual([])
  })

  test('every external section link opens a registered page', () => {
    const urls = externalSectionUrls()
    expect(urls).toContain('revenue')
    expect(urls).toContain('errors/alert-rules')
    expect(
      urls
        .map((url) => resolveUrl(PROJECT_ROUTES.external, url))
        .filter((r) => r !== 'ok')
    ).toEqual([])
  })

  test('alert rule create and edit pages resolve for both project kinds', () => {
    for (const routes of [PROJECT_ROUTES.hosted, PROJECT_ROUTES.external]) {
      expect(resolveUrl(routes, 'errors/alert-rules/new')).toBe('ok')
      const edit = matchRoutes(routes, '/errors/alert-rules/7/edit')
      expect((edit ?? []).slice(-1)[0]?.route.path).toBe(
        'errors/alert-rules/:ruleId/edit'
      )
    }
  })
})

describe('command palette project pages', () => {
  test('every entry opens a registered page', () => {
    expect(
      projectNavItems
        .map((item) => resolveUrl(PROJECT_ROUTES.hosted, item.url))
        .filter((r) => r !== 'ok')
    ).toEqual([])
  })

  test('an unknown hosted project URL says so instead of rendering nothing', () => {
    expect(
      resolveLeaf(PROJECT_ROUTES.hosted, 'workspace')?.route.elementName
    ).toBe('ProjectPageNotFound')
  })

  test('every hosted entry opens the canonical page, not a redirect', () => {
    expect(
      projectNavItems
        .map((item) => ({
          url: item.url,
          element: resolveLeaf(PROJECT_ROUTES.hosted, item.url)?.route
            .elementName,
        }))
        .filter(({ element }) => element && REDIRECT_ELEMENTS.has(element))
    ).toEqual([])
  })

  test('an external project is offered only the pages it registers', () => {
    const external = projectNavItemsFor({ source_type: 'external' })
    expect(external.length).toBeGreaterThan(5)
    expect(external.map((item) => item.url)).not.toContain('deployments')
    expect(external.map((item) => item.url)).not.toContain(
      'environment-variables'
    )
    for (const item of external) {
      expect(resolveUrl(PROJECT_ROUTES.external, item.url)).toBe('ok')
      // `settings/*` renders General settings for anything under it, so a
      // settings entry must land on its own route, not on that fallback.
      if (item.url.startsWith('settings/'))
        expect(resolveLeaf(PROJECT_ROUTES.external, item.url)?.route.path).toBe(
          item.url.split('?')[0]
        )
    }
  })

  test('hosting pages are not offered for an external project', () => {
    for (const item of projectNavItems) {
      if (isExternalProjectPage(item.url)) continue
      const leaf = resolveLeaf(PROJECT_ROUTES.external, item.url)
      // What such a URL would show: "Add hosting", or the general settings
      // page standing in for a hosting-only settings page.
      expect([`*`, 'settings/*']).toContain(leaf?.route.path ?? '*')
    }
    expect(projectNavItemsFor({ source_type: 'git' })).toEqual(projectNavItems)
  })

  test('Build & Deploy opens the combined delivery settings', () => {
    expect(
      projectNavItems.find((item) => item.title === 'Build & Deploy')?.url
    ).toBe('settings/delivery')
  })
})

describe('legacy routes', () => {
  test('each redirects to a page that renders', () => {
    for (const [legacy, target] of Object.entries(LEGACY_PROJECT_ROUTES)) {
      const element = resolveLeaf(PROJECT_ROUTES.hosted, legacy)?.route
        .elementName
      expect({ legacy, element }).toEqual({
        legacy,
        element: 'LegacyProjectRouteRedirect',
      })
      const landing = resolveLeaf(PROJECT_ROUTES.hosted, target)
      expect({ target, ok: resolveUrl(PROJECT_ROUTES.hosted, target) }).toEqual(
        { target, ok: 'ok' }
      )
      expect(REDIRECT_ELEMENTS.has(landing?.route.elementName ?? '')).toBe(
        false
      )
    }
  })

  test('no path is both redirected and rendered', () => {
    expect(duplicatePaths(PROJECT_ROUTES.hosted)).toEqual([])
    expect(duplicatePaths(PROJECT_ROUTES.external)).toEqual([])
    expect(
      duplicatePaths(
        readRouteTables('components/project/ProjectSettings.tsx').hosted[0]
      )
    ).toEqual([])
  })
})

describe('autopilot links', () => {
  test('land on the agent routes', () => {
    const paths = autopilotPaths('demo')
    const prefix = '/projects/demo/'
    for (const [url, pattern] of [
      [paths.agent('nightly'), 'agents/detail/:agentSlug'],
      [paths.editAgent('nightly'), 'agents/detail/:agentSlug/edit'],
      [paths.run(42), 'agents/:runId'],
    ]) {
      expect(url.startsWith(prefix)).toBe(true)
      const match = matchRoutes(
        PROJECT_ROUTES.hosted,
        url.slice(prefix.length - 1)
      )
      expect((match ?? []).slice(-1)[0]?.route.path).toBe(pattern)
    }
  })
})
