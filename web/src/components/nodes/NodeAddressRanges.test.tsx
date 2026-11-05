// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { NodeAddressRanges } from './NodeAddressRanges'

function text(html: string): string {
  return html.replace(/<[^>]+>/g, '').replace(/&#x27;|&#39;/g, "'")
}

describe('NodeAddressRanges', () => {
  test('says what each range is for and that they do not collide', () => {
    const shown = text(
      renderToStaticMarkup(
        <NodeAddressRanges
          meshCidr="10.201.0.0/16"
          computeCidr="10.205.0.0/16"
          subnetPrefixLen={24}
        />
      )
    )
    expect(shown).toContain('10.201.0.0/16: machines on the private network')
    expect(shown).toContain('one address from it, shown under Mesh.')
    // The two columns match only for outbound joins; say which.
    expect(shown).toContain('joined without an address of its own')
    expect(shown).toContain('so Node address shows the same one')
    expect(shown).toContain('--private-address')
    expect(shown).toContain(
      '10.205.0.0/16: container addresses, a /24 per node'
    )
    expect(shown).toContain('The two ranges never overlap')
  })

  test('while the private network starts, says the addresses will show', () => {
    const shown = text(
      renderToStaticMarkup(
        <NodeAddressRanges
          meshCidr="10.201.0.0/16"
          meshReady={false}
          computeCidr="10.205.0.0/16"
          subnetPrefixLen={24}
        />
      )
    )
    expect(shown).toContain('shown under Mesh once the network is up')
  })

  test('uses the ranges this cluster is configured with', () => {
    const shown = text(
      renderToStaticMarkup(
        <NodeAddressRanges
          meshCidr="172.30.0.0/16"
          computeCidr="10.90.0.0/16"
          subnetPrefixLen={26}
        />
      )
    )
    expect(shown).toContain('172.30.0.0/16: machines')
    expect(shown).toContain('10.90.0.0/16: container addresses, a /26 per node')
  })

  test('without the private network it only explains the container range', () => {
    const shown = text(
      renderToStaticMarkup(
        <NodeAddressRanges
          meshCidr={undefined}
          computeCidr="10.205.0.0/16"
          subnetPrefixLen={24}
        />
      )
    )
    expect(shown).toContain('10.205.0.0/16: container addresses')
    expect(shown).not.toContain('machines on the private network')
    expect(shown).not.toContain('never overlap')
  })

  test('renders nothing when neither range can be read', () => {
    expect(
      renderToStaticMarkup(
        <NodeAddressRanges
          meshCidr={undefined}
          computeCidr={undefined}
          subnetPrefixLen={undefined}
        />
      )
    ).toBe('')
  })
})
