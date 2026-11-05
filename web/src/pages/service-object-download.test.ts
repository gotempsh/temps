// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import {
  downloadServiceObject,
  objectBodyAsBlob,
} from './service-object-download'

function harness(fetchObject: () => Promise<unknown>) {
  const saved: Array<{ blob: Blob; fileName: string }> = []
  const failures: Array<{ title: string; description: string }> = []
  const run = () =>
    downloadServiceObject({
      fetchObject,
      save: (blob, fileName) => saved.push({ blob, fileName }),
      reportFailure: (title, description) =>
        failures.push({ title, description }),
      fileName: 'report.csv',
    })
  return { saved, failures, run }
}

describe('downloadServiceObject', () => {
  test('a refused download is reported with the server reason', async () => {
    const { saved, failures, run } = harness(() =>
      Promise.reject({
        type: 'about:blank',
        title: 'Not Found',
        status: 404,
        detail: 'Object report.csv not found in bucket uploads',
      })
    )

    expect(await run()).toBe(false)
    expect(saved).toHaveLength(0)
    expect(failures).toEqual([
      {
        title: 'Failed to download report.csv',
        description: 'Object report.csv not found in bucket uploads',
      },
    ])
  })

  test('an empty body is reported instead of saving nothing', async () => {
    const { saved, failures, run } = harness(() => Promise.resolve(undefined))

    expect(await run()).toBe(false)
    expect(saved).toHaveLength(0)
    expect(failures[0]?.description).toBe(
      'The server returned an empty response for this object.'
    )
  })

  test('a downloaded object is saved under its name', async () => {
    const body = new Blob(['a,b\n1,2\n'])
    const { saved, failures, run } = harness(() => Promise.resolve(body))

    expect(await run()).toBe(true)
    expect(failures).toHaveLength(0)
    expect(saved).toEqual([{ blob: body, fileName: 'report.csv' }])
  })
})

describe('objectBodyAsBlob', () => {
  test('wraps text bodies as bytes', async () => {
    expect(await objectBodyAsBlob('hello').text()).toBe('hello')
  })
})
