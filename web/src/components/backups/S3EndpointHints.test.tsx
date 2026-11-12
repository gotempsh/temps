// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { S3EndpointPreviewLine, S3PathStyleField } from './S3EndpointHints'

describe('S3EndpointPreviewLine', () => {
  test('a bare IP endpoint is previewed as plain HTTP', () => {
    const html = renderToStaticMarkup(
      <S3EndpointPreviewLine endpoint="10.0.0.5:9000" />
    )
    expect(html).toContain('http://10.0.0.5:9000')
    expect(html).toContain('over plain HTTP')
    expect(html).toContain('Type https:// to use TLS')
  })

  test('an empty endpoint renders nothing', () => {
    expect(renderToStaticMarkup(<S3EndpointPreviewLine endpoint="" />)).toBe('')
  })

  test('an unusable endpoint says what to enter', () => {
    expect(
      renderToStaticMarkup(<S3EndpointPreviewLine endpoint="ftp://10.0.0.5" />)
    ).toContain('Enter a host such as')
  })
})

describe('S3PathStyleField', () => {
  test('an IP endpoint shows path style forced on and why', () => {
    const html = renderToStaticMarkup(
      <S3PathStyleField
        id="ps"
        endpoint="10.0.0.5:9000"
        checked={false}
        onCheckedChange={() => {}}
      />
    )
    expect(html).toContain('data-state="checked"')
    expect(html).toContain('disabled')
    expect(html).toContain('Forced: the endpoint is an IP address')
  })

  test('a hostname leaves the box to the operator', () => {
    const html = renderToStaticMarkup(
      <S3PathStyleField
        id="ps"
        endpoint="https://s3.example.com"
        checked={false}
        onCheckedChange={() => {}}
      />
    )
    expect(html).toContain('data-state="unchecked"')
    expect(html).not.toContain('Forced:')
  })
})
