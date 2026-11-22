// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import { matchRoutes, type RouteObject } from 'react-router'
import ts from 'typescript'

import { groupConnectRepositoryHref, groupHref } from '@/lib/terminology'

/**
 * The console's routes live inline in `App.tsx`; read the `<Route>` elements
 * from the source the browser runs rather than a hand-kept copy.
 */
const APP = join(import.meta.dir, '..', 'App.tsx')

function readRoutes(): Array<RouteObject & { elementName?: string }> {
  const source = ts.createSourceFile(
    APP,
    readFileSync(APP, 'utf8'),
    ts.ScriptTarget.Latest,
    true,
    ts.ScriptKind.TSX
  )
  const routes: Array<RouteObject & { elementName?: string }> = []
  const visit = (node: ts.Node) => {
    if (
      ts.isJsxSelfClosingElement(node) &&
      node.tagName.getText() === 'Route'
    ) {
      const attrs = node.attributes.properties.filter(ts.isJsxAttribute)
      const path = attrs.find((a) => a.name.getText() === 'path')?.initializer
      const element = attrs.find(
        (a) => a.name.getText() === 'element'
      )?.initializer
      if (path && ts.isStringLiteral(path)) {
        const text = element?.getText() ?? ''
        routes.push({
          path: path.text,
          elementName: /<\s*([A-Za-z]+)/.exec(text)?.[1],
        })
      }
    }
    ts.forEachChild(node, visit)
  }
  visit(source)
  return routes
}

function leafOf(url: string) {
  const matches = matchRoutes(readRoutes(), url)
  return matches?.[matches.length - 1]?.route as
    (RouteObject & { elementName?: string }) | undefined
}

describe('project repository routes', () => {
  test('the connect-repository page has its own route', () => {
    const route = leafOf(groupConnectRepositoryHref('shop'))
    expect(route?.path).toBe('/p/:slug/repository')
    expect(route?.elementName).toBe('ConnectGroupRepository')
  })

  test('the project overview is not shadowed by it', () => {
    const route = leafOf(groupHref('shop'))
    expect(route?.path).toBe('/p/:slug')
    expect(route?.elementName).toBe('ProjectGroupDetail')
  })

  test('the import follow-up is carried in the search string only', () => {
    expect(groupConnectRepositoryHref('shop', { thenImport: true })).toBe(
      '/p/shop/repository?then=import'
    )
    expect(groupConnectRepositoryHref('shop')).toBe('/p/shop/repository')
  })
})
