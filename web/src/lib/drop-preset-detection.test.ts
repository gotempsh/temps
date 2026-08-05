import { describe, expect, test } from 'bun:test'
import { prepareAndInspectDrop } from './drop-preset-detection'

describe('prepareAndInspectDrop', () => {
  test('packages selected files and returns the detected preset', async () => {
    let inspectedArchive: File | undefined
    const result = await prepareAndInspectDrop(
      [
        {
          file: new File(['<h1>Hello</h1>'], 'index.html'),
          path: 'index.html',
        },
      ],
      undefined,
      async (archive) => {
        inspectedArchive = archive
        return {
          suggestedName: 'hello',
          candidates: [
            {
              confidence: 'high',
              directory: '.',
              isStatic: true,
              label: 'Static site',
              preset: 'static',
              reason: 'Found index.html',
            },
          ],
        }
      }
    )

    expect(inspectedArchive).toBeInstanceOf(File)
    expect(result.archive).toBe(inspectedArchive!)
    expect(result.inspection.candidates[0]?.preset).toBe('static')
  })

  test('rejects an inspection with no deployable candidates', async () => {
    await expect(
      prepareAndInspectDrop(
        [{ file: new File(['{}'], 'package.json'), path: 'package.json' }],
        undefined,
        async () => ({ suggestedName: 'empty', candidates: [] })
      )
    ).rejects.toThrow('No deployable project preset was detected')
  })

  test('cancels before uploading when the user clears the selection', async () => {
    const controller = new AbortController()
    controller.abort()
    let inspectCalled = false

    await expect(
      prepareAndInspectDrop(
        [{ file: new File(['{}'], 'package.json'), path: 'package.json' }],
        undefined,
        async () => {
          inspectCalled = true
          return { suggestedName: 'cancelled', candidates: [] }
        },
        { signal: controller.signal }
      )
    ).rejects.toMatchObject({ name: 'AbortError' })
    expect(inspectCalled).toBe(false)
  })
})
