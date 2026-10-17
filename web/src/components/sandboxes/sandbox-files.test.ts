// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  decodeFile,
  defaultFilePath,
  encodeText,
  filePropertiesArgv,
  filesUnavailableReason,
  formatFileMode,
  parseFileProperties,
  propertiesAfterSave,
  saveRisks,
  writeSemantics,
  writeMode,
  initialDirectory,
  isEditableFile,
  joinSandboxPath,
  parentDirectory,
  MAX_EDITABLE_BYTES,
  validateSandboxPath,
} from './sandbox-files'

describe('validateSandboxPath', () => {
  test('accepts an absolute file path', () => {
    expect(validateSandboxPath('/home/user/app/index.ts')).toBeNull()
  })

  test('explains each way a path is unusable', () => {
    expect(validateSandboxPath('  ')).toContain('Enter')
    expect(validateSandboxPath('app/index.ts')).toContain('absolute')
    expect(validateSandboxPath('/home/user/')).toContain('directory')
    expect(validateSandboxPath('/a\0b')).toContain('invalid')
    expect(validateSandboxPath(`/${'a'.repeat(5000)}`)).toContain('too long')
  })
})

describe('defaultFilePath', () => {
  test('starts the path in the work dir', () => {
    expect(defaultFilePath('/home/user/work')).toBe('/home/user/work/')
    expect(defaultFilePath('/home/user/work/')).toBe('/home/user/work/')
  })

  test('falls back to the root for a missing or relative work dir', () => {
    expect(defaultFilePath(undefined)).toBe('/')
    expect(defaultFilePath('workspace')).toBe('/')
  })
})

describe('encodeText / decodeFile', () => {
  test('round-trips ASCII and non-Latin-1 text', () => {
    for (const text of ['hello\n', 'héllo — 日本語 🚀', '']) {
      expect(decodeFile(encodeText(text))).toEqual({ ok: true, text })
    }
  })

  test('round-trips text larger than one encoding chunk', () => {
    const text = 'x'.repeat(100_000)
    expect(decodeFile(encodeText(text))).toEqual({ ok: true, text })
  })

  test('refuses binary content and invalid base64', () => {
    expect(decodeFile(btoa(String.fromCharCode(0x89, 0x50, 0, 0x47)))).toEqual({
      ok: false,
      reason: 'binary',
    })
    expect(decodeFile(btoa(String.fromCharCode(0xff, 0xfe, 0xfd)))).toEqual({
      ok: false,
      reason: 'binary',
    })
    expect(decodeFile('***not base64***')).toEqual({
      ok: false,
      reason: 'invalid',
    })
  })
})

describe('filesUnavailableReason', () => {
  test('is null while running and for a sleeping workspace', () => {
    expect(filesUnavailableReason({ status: 'running' })).toBeNull()
    expect(
      filesUnavailableReason({ status: 'stopped', lifecycle: 'workspace' })
    ).toBeNull()
  })

  test('explains stopped and destroyed sandboxes', () => {
    expect(filesUnavailableReason({ status: 'stopped' })).toContain('Resume')
    expect(filesUnavailableReason({ status: 'destroyed' })).toContain(
      'destroyed'
    )
  })
})

describe('isEditableFile', () => {
  const file = { exists: true, is_file: true, size: 10 }
  test('accepts a small regular file', () => {
    expect(isEditableFile(file)).toBe(true)
    expect(isEditableFile({ ...file, size: MAX_EDITABLE_BYTES })).toBe(true)
  })
  test('refuses missing, non-regular, oversized and unknown paths', () => {
    expect(isEditableFile({ ...file, exists: false })).toBe(false)
    expect(isEditableFile({ ...file, is_file: false })).toBe(false)
    expect(isEditableFile({ ...file, size: MAX_EDITABLE_BYTES + 1 })).toBe(
      false
    )
    expect(isEditableFile(undefined)).toBe(false)
  })
})

describe('file property probe', () => {
  test('follows symlinks (-L) and passes the path as $1, not in the script', () => {
    const argv = filePropertiesArgv('/a b/run.sh; rm -rf /')
    expect(argv.slice(0, 2)).toEqual(['sh', '-c'])
    const script = argv[2]!
    expect(script).toContain('stat -L -c')
    expect(script).not.toContain('rm -rf')
    expect(argv[argv.length - 1]).toBe('/a b/run.sh; rm -rf /')
  })

  test('parses mode, owner and the symlink marker', () => {
    expect(parseFileProperties('755 1000\n')).toEqual({
      mode: 0o755,
      uid: 1000,
      symlink: false,
    })
    expect(parseFileProperties('600 0\nsymlink\n')).toEqual({
      mode: 0o600,
      uid: 0,
      symlink: true,
    })
    expect(parseFileProperties('4755 1000')?.mode).toBe(0o4755)
  })

  test('rejects anything that is not the probe output', () => {
    expect(parseFileProperties('')).toBeNull()
    expect(parseFileProperties('stat: cannot stat')).toBeNull()
    expect(parseFileProperties('755')).toBeNull()
    expect(parseFileProperties('999 1000')).toBeNull()
    expect(parseFileProperties('75 1000')).toBeNull()
  })

  test('formats a mode the way chmod users write it', () => {
    expect(formatFileMode(0o755)).toBe('0755')
    expect(formatFileMode(0o600)).toBe('0600')
    expect(formatFileMode(0)).toBe('0000')
  })
})

describe('writeMode', () => {
  test('writes back the rwx bits only', () => {
    expect(writeMode({ mode: 0o755, uid: 1000, symlink: false })).toBe(0o755)
    expect(writeMode({ mode: 0o4755, uid: 1000, symlink: false })).toBe(0o755)
    expect(writeMode({ mode: 0o1777, uid: 0, symlink: false })).toBe(0o777)
  })

  test('falls back to 0644 when the mode is unknown', () => {
    expect(writeMode(null)).toBe(0o644)
  })
})

describe('saveRisks', () => {
  const file = (
    over: Partial<{ mode: number; uid: number; symlink: boolean }>
  ) => ({
    mode: 0o644,
    uid: 1000,
    symlink: false,
    ...over,
  })

  test('a plain sandbox-user file inside the home is a faithful save', () => {
    expect(
      saveRisks('/home/temps/workspace/a.sh', file({ mode: 0o755 }), 'docker')
    ).toEqual([])
  })

  test('warns when permissions are unknown', () => {
    expect(saveRisks('/home/temps/a', null, 'docker')[0]).toContain(
      'could not be read'
    )
  })

  test('warns that a symlink is replaced by a regular file', () => {
    const risks = saveRisks(
      '/home/temps/link',
      file({ symlink: true }),
      'docker'
    )
    expect(risks).toHaveLength(1)
    expect(risks[0]).toContain('symbolic link')
  })

  test('warns when a sandbox-user file outside the home would become root-owned', () => {
    const risks = saveRisks('/srv/app/.env', file({ mode: 0o600 }), 'docker')
    expect(risks).toHaveLength(1)
    expect(risks[0]).toContain('as root')
  })

  test('warns when a save would hand a file inside the home to the sandbox user', () => {
    // A root-owned 0644 under /home/temps is re-created as uid 1000.
    const risks = saveRisks(
      '/home/temps/workspace/conf',
      file({ uid: 0 }),
      'docker'
    )
    expect(risks).toHaveLength(1)
    expect(risks[0]).toContain('uid 1000')
    expect(risks[0]).toContain('currently owned by uid 0')
    // Some other account's file is handed over just the same.
    expect(
      saveRisks('/home/temps/x', file({ uid: 33 }), 'docker')
    ).toHaveLength(1)
  })

  test('follows the provider rule, a plain prefix test on the home', () => {
    // Mirrors `runtime_user_owns_uploaded_file`: /home/temps-other/x is
    // written as uid 1000 too, so a uid 1000 file there is a faithful save.
    expect(saveRisks('/home/temps-other/x', file({}), 'docker')).toEqual([])
    expect(
      saveRisks('/home/temps-other/x', file({ uid: 0 }), 'docker')
    ).toHaveLength(1)
  })

  test('a root-owned file outside the home is already root, so nothing changes', () => {
    expect(saveRisks('/etc/motd', file({ uid: 0 }), 'docker')).toEqual([])
  })

  test('warns that setuid/setgid/sticky bits are not kept', () => {
    const risks = saveRisks(
      '/home/temps/bin/tool',
      file({ mode: 0o4755 }),
      'docker'
    )
    expect(risks).toHaveLength(1)
    expect(risks[0]).toContain('setuid')
  })
})

describe('writeSemantics', () => {
  test('is known for the backends whose write path is in this repository', () => {
    expect(writeSemantics('docker')).toBe('recreate')
    expect(writeSemantics('firecracker')).toBe('in-place')
    expect(writeSemantics('local')).toBe('in-place')
  })

  test('is unknown for microsandbox and for legacy rows with no backend', () => {
    expect(writeSemantics('microsandbox')).toBe('unknown')
    expect(writeSemantics(null)).toBe('unknown')
    expect(writeSemantics(undefined)).toBe('unknown')
  })
})

describe('saveRisks by backend', () => {
  const file = (
    over: Partial<{ mode: number; uid: number; symlink: boolean }>
  ) => ({
    mode: 0o644,
    uid: 1000,
    symlink: false,
    ...over,
  })

  test('an in-place backend writes through a link and keeps the owner, so neither is warned about', () => {
    for (const backend of ['firecracker', 'local']) {
      expect(
        saveRisks('/home/temps/link', file({ symlink: true }), backend)
      ).toEqual([])
      expect(
        saveRisks('/srv/app/.env', file({ mode: 0o600 }), backend)
      ).toEqual([])
      expect(saveRisks('/home/temps/conf', file({ uid: 0 }), backend)).toEqual(
        []
      )
    }
  })

  test('the mode is written explicitly on every backend, so dropped setuid bits are always reported', () => {
    for (const backend of ['docker', 'firecracker', 'local', 'microsandbox']) {
      const risks = saveRisks(
        '/home/temps/bin/tool',
        file({ mode: 0o4755 }),
        backend
      )
      expect(risks).toHaveLength(1)
      expect(risks[0]).toContain('setuid')
    }
  })

  test('an unknown backend gets hedged wording, not claims about what happens', () => {
    const link = saveRisks(
      '/home/temps/link',
      file({ symlink: true }),
      'microsandbox'
    )
    expect(link).toHaveLength(1)
    expect(link[0]).toContain('Depending on the sandbox backend')
    expect(link[0]).not.toContain('Saving replaces the link')

    const owner = saveRisks('/srv/app/.env', file({}), null)
    expect(owner).toHaveLength(1)
    expect(owner[0]).toContain('may change who owns')
    expect(owner[0]).not.toContain('as root')
  })

  test('a file the write would leave as it is stays quiet on an unknown backend', () => {
    expect(saveRisks('/home/temps/a.sh', file({}), 'microsandbox')).toEqual([])
  })
})

describe('propertiesAfterSave', () => {
  const before = { mode: 0o644, uid: 33, symlink: true }

  test('a recreating backend gives the owner the path decides and drops the link', () => {
    expect(
      propertiesAfterSave('/home/temps/a', 0o600, 'docker', before)
    ).toEqual({ mode: 0o600, uid: 1000, symlink: false })
    expect(propertiesAfterSave('/tmp/a', 0o600, 'docker', before).uid).toBe(0)
  })

  test('an in-place backend keeps the previous owner and link status', () => {
    expect(
      propertiesAfterSave('/home/temps/a', 0o600, 'firecracker', before)
    ).toEqual({ mode: 0o600, uid: 33, symlink: true })
  })

  test('with nothing known beforehand it falls back to the path rule', () => {
    expect(propertiesAfterSave('/tmp/a', 0o600, 'firecracker', null)).toEqual({
      mode: 0o600,
      uid: 0,
      symlink: false,
    })
  })
})

describe('directory navigation helpers', () => {
  test('starts in the work dir, falling back to the root', () => {
    expect(initialDirectory('/home/temps/workspace/')).toBe(
      '/home/temps/workspace'
    )
    expect(initialDirectory('')).toBe('/')
    expect(initialDirectory(null)).toBe('/')
    expect(initialDirectory('relative/dir')).toBe('/')
  })

  test('goes up one level and stops at the root', () => {
    expect(parentDirectory('/home/temps/workspace')).toBe('/home/temps')
    expect(parentDirectory('/home/temps/workspace/')).toBe('/home/temps')
    expect(parentDirectory('/home')).toBe('/')
    expect(parentDirectory('/')).toBe('/')
  })

  test('joins a name onto a directory without doubling slashes', () => {
    expect(joinSandboxPath('/', 'etc')).toBe('/etc')
    expect(joinSandboxPath('/home/temps', 'app.ts')).toBe('/home/temps/app.ts')
    expect(joinSandboxPath('/home/temps/', 'app.ts')).toBe('/home/temps/app.ts')
  })
})
