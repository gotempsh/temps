// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { filesFromDrop } from './drop-files'

type FakeEntry = {
  isFile: boolean
  isDirectory: boolean
  name: string
  file?: (success: (file: File) => void) => void
  createReader?: () => {
    readEntries: (success: (entries: FakeEntry[]) => void) => void
  }
}

function fileEntry(name: string, onRead?: () => void): FakeEntry {
  return {
    isFile: true,
    isDirectory: false,
    name,
    file: (success) => {
      onRead?.()
      success(new File([name], name))
    },
  }
}

function directoryEntry(name: string, batches: FakeEntry[][]): FakeEntry {
  return {
    isFile: false,
    isDirectory: true,
    name,
    createReader: () => {
      let index = 0
      return {
        readEntries: (success) => success(batches[index++] ?? []),
      }
    },
  }
}

function dropEvent(
  ...entries: FakeEntry[]
): Parameters<typeof filesFromDrop>[0] {
  return {
    dataTransfer: {
      files: [],
      items: entries.map((entry) => ({
        webkitGetAsEntry: () => entry,
      })),
    },
  } as unknown as Parameters<typeof filesFromDrop>[0]
}

describe('filesFromDrop', () => {
  test('drains directory-reader batches while preserving nested paths', async () => {
    const root = directoryEntry('project', [
      [fileEntry('README.md')],
      [directoryEntry('src', [[fileEntry('index.ts')], []])],
      [],
    ])

    const files = await filesFromDrop(dropEvent(root), { maxEntries: 10 })

    expect(files.map((file) => file.path)).toEqual([
      'project/README.md',
      'project/src/index.ts',
    ])
  })

  test('skips ignored directories before enumerating their children', async () => {
    let dependencyReaderCreated = false
    const dependencies = directoryEntry('node_modules', [
      [fileEntry('dependency.js')],
      [],
    ])
    const originalCreateReader = dependencies.createReader
    dependencies.createReader = () => {
      dependencyReaderCreated = true
      return originalCreateReader!()
    }
    const skipped: string[] = []
    const root = directoryEntry('project', [
      [dependencies, fileEntry('package.json')],
      [],
    ])

    const files = await filesFromDrop(dropEvent(root), {
      maxEntries: 10,
      shouldSkipPath: (path) => path.split('/').includes('node_modules'),
      onSkippedPath: (path) => skipped.push(path),
    })

    expect(dependencyReaderCreated).toBe(false)
    expect(files.map((file) => file.path)).toEqual(['project/package.json'])
    expect(skipped).toEqual(['project/node_modules'])
  })

  test('stops reading file contents as soon as the entry cap is crossed', async () => {
    let filesRead = 0
    const root = directoryEntry('project', [
      [
        fileEntry('one.txt', () => filesRead++),
        fileEntry('two.txt', () => filesRead++),
        fileEntry('three.txt', () => filesRead++),
      ],
      [],
    ])

    await expect(
      filesFromDrop(dropEvent(root), { maxEntries: 3 })
    ).rejects.toThrow('3 entry reading limit')
    expect(filesRead).toBe(2)
  })

  test('counts empty directories against the traversal cap', async () => {
    const root = directoryEntry('project', [
      [directoryEntry('one', [[]]), directoryEntry('two', [[]])],
      [],
    ])

    await expect(
      filesFromDrop(dropEvent(root), { maxEntries: 2 })
    ).rejects.toThrow('2 entry reading limit')
  })

  test('stops traversal when the caller aborts', async () => {
    const controller = new AbortController()
    let filesRead = 0
    const root = directoryEntry('project', [
      [
        fileEntry('one.txt', () => {
          filesRead += 1
          controller.abort()
        }),
        fileEntry('two.txt', () => filesRead++),
      ],
      [],
    ])

    await expect(
      filesFromDrop(dropEvent(root), { signal: controller.signal })
    ).rejects.toThrow()
    expect(filesRead).toBe(1)
  })
})
