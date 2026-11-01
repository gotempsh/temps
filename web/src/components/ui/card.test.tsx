// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { SettingsSurface, saveRowClassName } from '@temps-sdk/ds'
import { Card, CardContent, CardFooter, CardHeader, CardTitle } from './card'

const framed = (html: string) => html.split('rounded-lg border').length - 1

function card(label: string, children?: React.ReactNode) {
  return (
    <Card data-card={label}>
      <CardHeader>
        <CardTitle>{label}</CardTitle>
      </CardHeader>
      <CardContent>{children}</CardContent>
    </Card>
  )
}

test('a card keeps its frame outside settings', () => {
  const html = renderToStaticMarkup(card('Standalone'))
  expect(framed(html)).toBe(1)
  expect(html).toContain('text-2xl')
})

test('a card placed in a settings group drops its frame, not its content', () => {
  const html = renderToStaticMarkup(
    <SettingsSurface>{card('Where it runs', 'Options')}</SettingsSurface>
  )
  expect(html).toContain('border-0')
  expect(html).toContain('Options')
  // Its title becomes a sub-heading under the group's own heading.
  expect(html).toContain('text-sm font-semibold')
  expect(html).not.toContain('text-2xl')
})

test('only the outermost card on a settings surface goes flat', () => {
  const html = renderToStaticMarkup(
    <SettingsSurface>{card('Outer', card('Inner'))}</SettingsSurface>
  )
  expect(html.split('border-0').length - 1).toBe(1)
})

test('a dialog-style reset gives cards their frame back', () => {
  const html = renderToStaticMarkup(
    <SettingsSurface>
      <SettingsSurface active={false}>{card('In a dialog')}</SettingsSurface>
    </SettingsSurface>
  )
  expect(html).not.toContain('border-0')
})

test('on a settings surface a card footer is the save row pages use', () => {
  const footer = (
    <Card>
      <CardContent>Fields</CardContent>
      <CardFooter>
        <button type="submit">Save</button>
      </CardFooter>
    </Card>
  )
  const flat = renderToStaticMarkup(<SettingsSurface>{footer}</SettingsSurface>)
    .replace(/&amp;/g, '&')
    .replace(/&gt;/g, '>')
  for (const name of saveRowClassName.split(' ')) expect(flat).toContain(name)

  const standalone = renderToStaticMarkup(footer)
  expect(standalone).not.toContain('border-t')
})
