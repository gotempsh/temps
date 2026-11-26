// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps_deploy_folder`: deploy a project folder on this computer in one
 * step. The extension's bridge answers this tool itself, because only it runs
 * where the files are; the Temps server never reads a path it is given.
 *
 * The model chooses the path, so a prompt it read could ask for any folder.
 * What may leave this computer is decided here, not by the model:
 * - Only folders inside one the user allowed (the extension's settings, or
 *   `temps mcp add --deploy-folder`), after
 *   resolving symlinks; never the home folder or a filesystem root itself.
 * - Symlinks inside the folder are left out, and each file is opened without
 *   following one and must be the very file listed (same device and inode),
 *   so a folder swapped for a symlink meanwhile does not redirect a read
 *   outside the folder.
 * - Version control, dependencies, build caches, `.env` files, keys and
 *   credentials are always left out, as is anything a `.gitignore` lists,
 *   including those above the folder up to its repository's root.
 * - At most LIMITS.files files and LIMITS.bytes bytes, and nothing is read
 *   into memory past those bytes (a `.gitignore` past LIMITS.ignoreBytes is
 *   refused unread).
 * Then it uploads a ZIP to the endpoint `temps deploy` uses. The server checks
 * the key there: it must be allowed to deploy that project. Claude asks the
 * user before running the tool (it is marked as making changes).
 */

'use strict'

const fs = require('node:fs')
const os = require('node:os')
const path = require('node:path')
const zlib = require('node:zlib')
const crypto = require('node:crypto')
const { promisify } = require('node:util')

const fsp = fs.promises
const deflateRaw = promisify(zlib.deflateRaw)

const DEPLOY_TOOL_NAME = 'temps_deploy_folder'
const LIMITS = {
  files: 10000,
  bytes: 100 * 1024 * 1024,
  ignoreBytes: 1024 * 1024,
}

const DEPLOY_TOOL = {
  name: DEPLOY_TOOL_NAME,
  title: 'Deploy a folder',
  description:
    "Deploy a project folder on the user's computer to a Temps project in one step: it uploads the source and Temps builds and deploys it. The folder must be inside one the user allowed for Temps deploys. It leaves out .git, node_modules, build caches, .env files, keys and whatever .gitignore lists. The project must accept uploaded source (a project created for upload does). Find project_id, and environment_id if the project has several environments, with the temps tool. Returns the deployment id; follow the build with the temps tool.",
  inputSchema: {
    type: 'object',
    properties: {
      path: {
        type: 'string',
        description:
          'Full path of the project folder on this computer, such as /Users/me/code/my-app or ~/code/my-app.',
      },
      project_id: {
        type: 'integer',
        minimum: 1,
        description: 'The Temps project to deploy to.',
      },
      environment_id: {
        type: 'integer',
        minimum: 1,
        description:
          'The environment to deploy to. Optional: by default the production environment, or the only one.',
      },
    },
    required: ['path', 'project_id'],
    additionalProperties: false,
  },
  annotations: {
    title: 'Deploy a folder',
    readOnlyHint: false,
    destructiveHint: true,
    idempotentHint: false,
    openWorldHint: false,
  },
}

/** Directories that are never uploaded: version control, dependencies, build output and caches. */
const LEFT_OUT_DIRECTORIES = new Set([
  '.git',
  '.hg',
  '.svn',
  'node_modules',
  '.next',
  '.nuxt',
  '.output',
  '.svelte-kit',
  '.turbo',
  '.vercel',
  '.netlify',
  '.cache',
  '__pycache__',
  '.venv',
  '.terraform',
])

/** Files that hold secrets more often than not. */
const SECRET_FILE_NAMES = new Set([
  '.envrc',
  '.netrc',
  '.pypirc',
  '.git-credentials',
  '.htpasswd',
  'credentials.json',
  '.DS_Store',
])
const SECRET_EXTENSIONS = new Set([
  '.pem',
  '.key',
  '.p12',
  '.pfx',
  '.jks',
  '.keystore',
  '.ppk',
])
// Package manager settings, left out only when they carry a credential.
const REGISTRY_SETTINGS = new Set(['.npmrc', '.yarnrc', '.yarnrc.yml'])
const REGISTRY_CREDENTIAL = /_auth|_password|npmAuthToken|npmAuthIdent/i

/** Whether a file or directory is left out whatever `.gitignore` says. */
function alwaysLeftOut(name, isDirectory) {
  if (isDirectory) return LEFT_OUT_DIRECTORIES.has(name)
  if (SECRET_FILE_NAMES.has(name)) return true
  // .env, .env.local, .env.production; examples hold no secrets and some
  // builds read them.
  if (/^\.env(\..+)?$/i.test(name)) {
    return !/^\.env\.(example|sample|template|dist)$/i.test(name)
  }
  if (/^id_(rsa|dsa|ecdsa|ed25519)/.test(name)) return true
  if (/^terraform\.tfstate/.test(name)) return true
  return SECRET_EXTENSIONS.has(path.extname(name).toLowerCase())
}

function escapeRegex(char) {
  return /[.*+?^${}()|[\]\\/]/.test(char) ? `\\${char}` : char
}

/** A `.gitignore` glob as a regular expression body. */
function globToRegex(glob) {
  let out = ''
  for (let i = 0; i < glob.length; i++) {
    const char = glob[i]
    if (char === '*') {
      if (glob[i + 1] === '*') {
        const atStart = i === 0 || glob[i - 1] === '/'
        if (atStart && glob[i + 2] === '/') {
          out += '(?:.*/)?'
          i += 2
          continue
        }
        if (atStart && i + 2 === glob.length) {
          out += '.*'
          i += 1
          continue
        }
        out += '[^/]*'
        i += 1
        continue
      }
      out += '[^/]*'
    } else if (char === '?') {
      out += '[^/]'
    } else if (char === '[') {
      const end = glob.indexOf(']', i + 2)
      if (end === -1) {
        out += '\\['
        continue
      }
      let members = glob.slice(i + 1, end).replace(/\\/g, '\\\\')
      if (members[0] === '!') members = `^${members.slice(1)}`
      out += `[${members}]`
      i = end
    } else if (char === '\\' && i + 1 < glob.length) {
      out += escapeRegex(glob[i + 1])
      i += 1
    } else {
      out += escapeRegex(char)
    }
  }
  return out
}

/**
 * The rules of one `.gitignore`. `base` is the folder it is in, relative to
 * the project, with a trailing slash ('' at the top).
 */
function parseIgnore(text, base) {
  const rules = []
  for (const raw of text.split(/\r?\n/)) {
    let line = raw.replace(/(?<!\\)\s+$/, '')
    if (!line || line.startsWith('#')) continue
    let negate = false
    if (line.startsWith('!')) {
      negate = true
      line = line.slice(1)
    } else if (line.startsWith('\\!') || line.startsWith('\\#')) {
      line = line.slice(1)
    }
    let directoryOnly = false
    if (line.endsWith('/')) {
      directoryOnly = true
      line = line.slice(0, -1)
    }
    const anchored = line.includes('/')
    if (line.startsWith('/')) line = line.slice(1)
    if (!line) continue
    let regex
    try {
      const body = globToRegex(line)
      regex = new RegExp(anchored ? `^${body}$` : `(?:^|/)${body}$`)
    } catch {
      continue // A pattern git would not understand either.
    }
    rules.push({ base, negate, directoryOnly, regex })
  }
  return rules
}

/** Whether `.gitignore` rules leave out a path (relative, '/'-separated). The last match wins. */
function isIgnored(rules, relative, isDirectory) {
  let ignored = false
  for (const rule of rules) {
    if (rule.directoryOnly && !isDirectory) continue
    if (!relative.startsWith(rule.base)) continue
    if (rule.regex.test(relative.slice(rule.base.length))) {
      ignored = !rule.negate
    }
  }
  return ignored
}

function megabytes(bytes) {
  if (bytes < 1024 * 1024) return `${Math.max(1, Math.round(bytes / 1024))} KB`
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`
}

/**
 * Where the user changes the bridge's settings, for the messages that say
 * what to do: the Claude app extension, or an app the Temps CLI configured
 * (`temps mcp add`), which starts the bridge with TEMPS_BRIDGE_HOST=cli.
 */
const HOSTS = {
  extension: {
    settings: 'the extension settings',
    folders: 'the folders Claude may deploy',
    noFolders:
      'No folder may be deployed yet. In Claude, open Settings, then Extensions, then Temps, add the project folder (or a folder above it) under "Folders Claude may deploy", and try again.',
    addFolder:
      'To allow it, add it or a folder above it in Claude: Settings, then Extensions, then Temps.',
  },
  cli: {
    settings: 'the app’s Temps MCP settings (run temps mcp add again)',
    folders: 'the folders this agent may deploy',
    noFolders:
      'No folder may be deployed yet. Run temps mcp add <app> --deploy-folder <folder> with the folder that holds your projects, restart the app, and try again.',
    addFolder:
      'To allow it, run temps mcp add <app> --deploy-folder <folder> with it or a folder above it, and restart the app.',
  },
}

/** The host whose wording the bridge uses, from its environment. */
function hostFor(env) {
  return env && env.TEMPS_BRIDGE_HOST === 'cli' ? HOSTS.cli : HOSTS.extension
}

/**
 * The folder to deploy, resolved, after checking it is inside a folder the
 * user allowed. Throws an Error that says what to do instead.
 */
async function allowedRoot(input, folders, host = HOSTS.extension) {
  if (folders.length === 0) throw new Error(host.noFolders)
  let requested = input.trim()
  if (requested === '~' || requested.startsWith('~/')) {
    requested = path.join(os.homedir(), requested.slice(1))
  }
  if (!path.isAbsolute(requested)) {
    throw new Error(
      `"${input}" is not a full path. Give the full path of the project folder, such as ${path.join(os.homedir(), 'code', 'my-app')}.`
    )
  }
  let root
  try {
    root = await fsp.realpath(requested)
  } catch (error) {
    if (error && error.code === 'ENOENT') {
      throw new Error(`${requested} does not exist on this computer.`, {
        cause: error,
      })
    }
    throw new Error(`Could not open ${requested}: ${error.message}`, {
      cause: error,
    })
  }
  if (!(await fsp.stat(root)).isDirectory()) {
    throw new Error(`${requested} is a file. Give the project's folder.`)
  }
  if (root === path.parse(root).root || root === os.homedir()) {
    throw new Error(
      `${requested} is not a project folder. Give the folder of the project itself.`
    )
  }
  for (const folder of folders) {
    let allowed
    try {
      allowed = await fsp.realpath(folder)
    } catch {
      continue // An allowed folder that no longer exists allows nothing.
    }
    const relative = path.relative(allowed, root)
    if (
      relative === '' ||
      (relative.split(path.sep)[0] !== '..' && !path.isAbsolute(relative))
    ) {
      // A folder packing always leaves out (.git, node_modules) is never a
      // project root either: deploying it would send what every other
      // deploy keeps back, such as the repository's history and hooks.
      const leftOut = relative
        .split(path.sep)
        .find((segment) => segment && alwaysLeftOut(segment, true))
      if (leftOut) {
        throw new Error(
          `${requested} is inside ${leftOut}, which is never deployed. Give the folder of the project itself.`
        )
      }
      return root
    }
  }
  throw new Error(
    `${requested} is outside ${host.folders} (${folders.join(', ')}). ${host.addFolder}`
  )
}

/**
 * Read at most `max` bytes of the regular file at `file`, without following a
 * symlink in its place. Throws `tooLarge(size)` for a larger file, before
 * reading it, or when it grew past `max` while it was read.
 */
async function readBounded(file, max, tooLarge) {
  const noFollow = fs.constants.O_NOFOLLOW || 0
  const handle = await fsp.open(file, fs.constants.O_RDONLY | noFollow)
  try {
    const stat = await handle.stat()
    if (!stat.isFile()) return null
    if (stat.size > max) throw tooLarge(stat.size)
    return await readHandle(handle, stat.size, max, tooLarge, file)
  } finally {
    await handle.close()
  }
}

/**
 * The bytes of an open file expected to hold `size` bytes, reading at most
 * one byte more to notice growth. Throws `tooLarge(read)` past `max`, and an
 * Error saying the file changed when it holds more than `size`.
 */
async function readHandle(handle, size, max, tooLarge, file) {
  const capacity = Math.min(size, max) + 1
  const buffer = Buffer.alloc(capacity)
  let read = 0
  while (read < capacity) {
    const { bytesRead } = await handle.read(buffer, read, capacity - read, read)
    if (bytesRead === 0) break
    read += bytesRead
  }
  if (read > max) throw tooLarge(read)
  if (read > size) {
    throw new Error(
      `${file} changed while it was read. Try again once the folder has stopped changing.`
    )
  }
  return buffer.subarray(0, read)
}

/** The rules of the `.gitignore` in `directory`, read within the limit. */
async function readIgnore(directory, base, limits) {
  const file = path.join(directory, '.gitignore')
  const text = await readBounded(
    file,
    limits.ignoreBytes,
    (size) =>
      new Error(
        `${file} is ${megabytes(size)}, more than the ${megabytes(limits.ignoreBytes)} a .gitignore may be here. Shorten it, then try again.`
      )
  )
  return text ? parseIgnore(text.toString('utf8'), base) : []
}

/**
 * The repository `root` is in, when it is below one: the nearest folder above
 * it with a `.git`. Null when there is none, or `root` is the repository's top.
 */
async function repositoryAbove(root) {
  const isRepository = async (directory) => {
    try {
      await fsp.lstat(path.join(directory, '.git'))
      return true
    } catch {
      return false
    }
  }
  if (await isRepository(root)) return null
  let directory = path.dirname(root)
  for (;;) {
    if (await isRepository(directory)) return directory
    const parent = path.dirname(directory)
    if (parent === directory) return null
    directory = parent
  }
}

/**
 * The `.gitignore` rules that apply to `root` from the folders above it, up to
 * its repository's top (git applies those too), with the path of `root` in
 * that repository ('apps/site/'). `ignored` names the first folder on the way
 * that one of them leaves out, in which case nothing under `root` is uploaded.
 */
async function rulesAbove(root, limits) {
  const repository = await repositoryAbove(root)
  if (!repository) return { prefix: '', rules: [], ignored: null }
  const segments = path.relative(repository, root).split(path.sep)
  let rules = []
  let directory = repository
  let base = ''
  for (const segment of segments) {
    rules = rules.concat(await readIgnoreIfFile(directory, base, limits))
    const relative = `${base}${segment}`
    if (isIgnored(rules, relative, true)) {
      return { prefix: `${relative}/`, rules, ignored: `${relative}/` }
    }
    directory = path.join(directory, segment)
    base = `${relative}/`
  }
  return { prefix: base, rules, ignored: null }
}

/** `readIgnore` for a folder that may have no `.gitignore`. */
async function readIgnoreIfFile(directory, base, limits) {
  try {
    const stat = await fsp.lstat(path.join(directory, '.gitignore'))
    if (!stat.isFile()) return []
  } catch {
    return []
  }
  return readIgnore(directory, base, limits)
}

/**
 * Whether `absolute` is, now, the file `stat` described inside the folder:
 * no symlink anywhere on its path (its real path is itself), the same file on
 * a second look, and, when given, its directory still the one listed. A
 * directory swapped for a symlink between listing and the file's lstat makes
 * that lstat describe a file outside; this refuses it rather than record the
 * outside file under an in-folder name.
 */
async function stillInside(absolute, stat, directory, listed) {
  try {
    if ((await fsp.realpath(absolute)) !== absolute) return false
    const again = await fsp.lstat(absolute)
    if (!again.isFile() || again.ino !== stat.ino || again.dev !== stat.dev) {
      return false
    }
    if (!listed) return true
    const now = await fsp.lstat(directory)
    return now.isDirectory() && now.ino === listed.ino && now.dev === listed.dev
  } catch {
    return false
  }
}

/**
 * Where an open file is, from the kernel, when the platform says (Linux);
 * null elsewhere.
 */
async function openedPath(handle) {
  if (process.platform !== 'linux') return null
  try {
    return await fsp.readlink(`/proc/self/fd/${handle.fd}`)
  } catch {
    return null
  }
}

/**
 * The files to upload under `root`, and what was left out. Throws when the
 * folder is over the limits, naming what to leave out. Each file is recorded
 * with its device and inode, which `packZip` checks the file it opens against.
 */
async function collectFiles(root, limits = LIMITS) {
  limits = { ...LIMITS, ...limits }
  const files = []
  const leftOut = []
  let total = 0

  // Paths in `.gitignore` rules are from the repository's top; `prefix` is
  // where `root` is in it.
  const above = await rulesAbove(root, limits)
  if (above.ignored) {
    leftOut.push(
      `${path.basename(root)}/ (${above.ignored} is in a .gitignore above it)`
    )
    return { files, leftOut, total }
  }
  const { prefix } = above

  const walk = async (directory, relative, rules) => {
    // A directory swapped for a symlink since it was listed is not followed,
    // and one reached through a symlink above it (its real path is not where
    // it was listed) is not read.
    const before = await fsp.lstat(directory)
    if (!before.isDirectory()) return
    const entries = await fsp.readdir(directory, { withFileTypes: true })
    const after = await fsp.lstat(directory)
    if (
      !after.isDirectory() ||
      after.ino !== before.ino ||
      after.dev !== before.dev ||
      (await fsp.realpath(directory)) !== directory
    ) {
      throw new Error(
        `${directory} changed while it was read. Try again once the folder has stopped changing.`
      )
    }
    entries.sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0))
    if (entries.some((e) => e.name === '.gitignore' && e.isFile())) {
      rules = rules.concat(
        await readIgnore(directory, `${prefix}${relative}`, limits)
      )
    }
    for (const entry of entries) {
      const name = `${relative}${entry.name}`
      if (entry.isSymbolicLink()) {
        leftOut.push(name)
        continue
      }
      const isDirectory = entry.isDirectory()
      if (!isDirectory && !entry.isFile()) continue
      if (
        alwaysLeftOut(entry.name, isDirectory) ||
        isIgnored(rules, `${prefix}${name}`, isDirectory)
      ) {
        leftOut.push(isDirectory ? `${name}/` : name)
        continue
      }
      const absolute = path.join(directory, entry.name)
      if (isDirectory) {
        await walk(absolute, `${name}/`, rules)
        continue
      }
      const stat = await fsp.lstat(absolute)
      if (!stat.isFile()) continue
      if (!(await stillInside(absolute, stat, directory, before))) {
        throw new Error(
          `${name} changed while the folder was read. Try again once the folder has stopped changing.`
        )
      }
      files.push({
        name,
        absolute,
        size: stat.size,
        mode: stat.mode,
        dev: stat.dev,
        ino: stat.ino,
      })
      total += stat.size
      if (files.length > limits.files) {
        throw new Error(
          `${root} has more than ${limits.files} files to upload. Add what the build doesn't need (generated files, data, media) to .gitignore, then try again.`
        )
      }
      if (total > limits.bytes) {
        throw new Error(
          `${root} has more than ${megabytes(limits.bytes)} to upload. Add large files the build doesn't need to .gitignore, then try again.`
        )
      }
    }
  }

  await walk(root, '', above.rules)
  return { files, leftOut, total }
}

const CRC_TABLE = (() => {
  const table = new Uint32Array(256)
  for (let n = 0; n < 256; n++) {
    let c = n
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1
    table[n] = c >>> 0
  }
  return table
})()

function crc32(bytes) {
  let crc = 0xffffffff
  for (let i = 0; i < bytes.length; i++) {
    crc = CRC_TABLE[(crc ^ bytes[i]) & 0xff] ^ (crc >>> 8)
  }
  return (crc ^ 0xffffffff) >>> 0
}

// 1980-01-01 00:00, the earliest ZIP time. Fixed, so the same folder makes the
// same archive.
const DOS_TIME = 0
const DOS_DATE = (0 << 9) | (1 << 5) | 1

/**
 * Read each file, without following a symlink put in its place since it was
 * listed, and pack them into a ZIP. Returns the archive and the files it
 * really holds (a registry settings file with a credential is left out here).
 */
async function packZip(collected, limits = LIMITS) {
  limits = { ...LIMITS, ...limits }
  const locals = []
  const central = []
  const packed = []
  let offset = 0
  let read = 0
  const noFollow = fs.constants.O_NOFOLLOW || 0
  for (const file of collected.files) {
    const handle = await fsp.open(
      file.absolute,
      fs.constants.O_RDONLY | noFollow
    )
    const grew = () =>
      new Error(
        `The folder grew past ${megabytes(limits.bytes)} while it was packed. Try again once it has stopped changing.`
      )
    let data
    try {
      const stat = await handle.stat()
      if (!stat.isFile()) continue
      // The file listed, not another one a folder swapped for a symlink
      // above it now leads to (O_NOFOLLOW covers only the last part).
      // And that file is still where it was listed, with no symlink on the
      // way: the kernel's path for what was opened when it gives one.
      const opened = await openedPath(handle)
      if (
        file.ino === undefined ||
        stat.ino !== file.ino ||
        stat.dev !== file.dev ||
        (opened !== null && opened !== file.absolute) ||
        !(await stillInside(file.absolute, stat))
      ) {
        throw new Error(
          `${file.name} changed while the folder was packed. Try again once it has stopped changing.`
        )
      }
      const remaining = limits.bytes - read
      if (stat.size > remaining) throw grew()
      data = await readHandle(handle, stat.size, remaining, grew, file.name)
    } finally {
      await handle.close()
    }
    read += data.length
    if (
      REGISTRY_SETTINGS.has(path.basename(file.name)) &&
      REGISTRY_CREDENTIAL.test(data.toString('utf8'))
    ) {
      collected.leftOut.push(file.name)
      continue
    }
    const deflated = await deflateRaw(data)
    const stored = deflated.length >= data.length
    const body = stored ? data : deflated
    const name = Buffer.from(file.name, 'utf8')
    const crc = crc32(data)
    const method = stored ? 0 : 8
    // rw-r--r--, or rwxr-xr-x for an executable, as a regular file.
    const mode = (file.mode & 0o111 ? 0o755 : 0o644) | 0o100000

    const local = Buffer.alloc(30)
    local.writeUInt32LE(0x04034b50, 0)
    local.writeUInt16LE(20, 4)
    local.writeUInt16LE(0x0800, 6) // UTF-8 names
    local.writeUInt16LE(method, 8)
    local.writeUInt16LE(DOS_TIME, 10)
    local.writeUInt16LE(DOS_DATE, 12)
    local.writeUInt32LE(crc, 14)
    local.writeUInt32LE(body.length, 18)
    local.writeUInt32LE(data.length, 22)
    local.writeUInt16LE(name.length, 26)
    local.writeUInt16LE(0, 28)

    const entry = Buffer.alloc(46)
    entry.writeUInt32LE(0x02014b50, 0)
    entry.writeUInt16LE((3 << 8) | 20, 4) // made on Unix, so the mode counts
    entry.writeUInt16LE(20, 6)
    entry.writeUInt16LE(0x0800, 8)
    entry.writeUInt16LE(method, 10)
    entry.writeUInt16LE(DOS_TIME, 12)
    entry.writeUInt16LE(DOS_DATE, 14)
    entry.writeUInt32LE(crc, 16)
    entry.writeUInt32LE(body.length, 20)
    entry.writeUInt32LE(data.length, 24)
    entry.writeUInt16LE(name.length, 28)
    entry.writeUInt32LE((mode << 16) >>> 0, 38)
    entry.writeUInt32LE(offset, 42)

    locals.push(local, name, body)
    central.push(entry, name)
    packed.push(file.name)
    offset += local.length + name.length + body.length
  }
  const directory = Buffer.concat(central)
  const end = Buffer.alloc(22)
  end.writeUInt32LE(0x06054b50, 0)
  end.writeUInt16LE(packed.length, 8)
  end.writeUInt16LE(packed.length, 10)
  end.writeUInt32LE(directory.length, 12)
  end.writeUInt32LE(offset, 16)
  return {
    zip: Buffer.concat([...locals, directory, end]),
    packed,
    bytes: read,
  }
}

function positiveId(value, field) {
  const number =
    typeof value === 'string' && /^\d+$/.test(value.trim())
      ? Number(value)
      : value
  if (!Number.isInteger(number) || number < 1 || number > 2147483647) {
    throw new Error(`${field} must be a Temps id, a whole number such as 4.`)
  }
  return number
}

function readArguments(args) {
  const input = args && typeof args === 'object' ? args : {}
  if (typeof input.path !== 'string' || !input.path.trim()) {
    throw new Error(
      'Give path: the full path of the project folder on this computer.'
    )
  }
  return {
    path: input.path,
    projectId: positiveId(input.project_id, 'project_id'),
    environmentId:
      input.environment_id === undefined || input.environment_id === null
        ? null
        : positiveId(input.environment_id, 'environment_id'),
  }
}

function rejectedKey(origin, host = HOSTS.extension) {
  return `Temps at ${origin} rejected the API key. Create a new key in the Temps console and update it in ${host.settings}.`
}

function problemDetail(text) {
  try {
    const problem = JSON.parse(text)
    return String(problem.detail || problem.title || '').slice(0, 500)
  } catch {
    return ''
  }
}

/** The JSON body of a successful API answer, or an Error that says what went wrong. */
function apiJson(res, config, action) {
  const { origin } = config
  const host = config.host || HOSTS.extension
  if (res.status >= 300 && res.status < 400) {
    throw new Error(
      `Temps at ${origin} redirected the request. The extension does not follow redirects, so your key only goes to the address you set.`
    )
  }
  if (res.status === 401) throw new Error(rejectedKey(origin, host))
  if (res.status < 200 || res.status >= 300) {
    const detail = problemDetail(res.text)
    if (res.status === 403) {
      throw new Error(
        `The API key may not ${action}${detail ? `: ${detail}` : '.'} Use a key that can make changes: create one in the Temps console and update it in ${host.settings}.`
      )
    }
    if (res.status === 404) {
      throw new Error(
        `Could not ${action}: ${detail || 'not found'}. Check the ids with the temps tool.`
      )
    }
    throw new Error(
      `Could not ${action}: Temps at ${origin} answered ${res.status}${detail ? `: ${detail}` : '.'}`
    )
  }
  const type = String(res.headers['content-type'] || '').toLowerCase()
  try {
    if (!type.includes('json')) throw new Error('not JSON')
    return JSON.parse(res.text)
  } catch {
    throw new Error(
      `${origin} answered with ${type ? type.split(';')[0] : 'something'} instead of the Temps API. Set the server address to the address of your Temps console.`
    )
  }
}

/** The environment to deploy to: the one asked for, production, or the only one. */
async function chooseEnvironment(api, config, projectId, environmentId) {
  if (environmentId !== null) return { id: environmentId, name: null }
  const action = `list the environments of project ${projectId}`
  const list = apiJson(
    await api('GET', `/api/projects/${projectId}/environments`),
    config,
    action
  )
  const standing = (Array.isArray(list) ? list : []).filter(
    (env) => env && Number.isInteger(env.id) && !env.is_preview
  )
  const production = standing.find(
    (env) =>
      env.slug === 'production' ||
      String(env.name || '').toLowerCase() === 'production'
  )
  const chosen = production || (standing.length === 1 ? standing[0] : null)
  if (chosen) return { id: chosen.id, name: chosen.name || null }
  if (standing.length === 0) {
    throw new Error(
      `Project ${projectId} has no environment to deploy to, or there is no project ${projectId}. Check the id with the temps tool.`
    )
  }
  throw new Error(
    `Project ${projectId} has several environments and none is production. Pass environment_id: ${standing.map((env) => `${env.name} (${env.id})`).join(', ')}.`
  )
}

function leftOutLine(leftOut) {
  if (leftOut.length === 0) return 'Nothing was left out.'
  const shown = leftOut.slice(0, 12).join(', ')
  const more = leftOut.length > 12 ? ` and ${leftOut.length - 12} more` : ''
  return `Left out: ${shown}${more}.`
}

/**
 * Run the tool. `api(method, path, headers?, body?)` sends a request to the
 * Temps server with the key and resolves to `{ status, headers, text }`.
 * Resolves to the text Claude shows; throws an Error with what to do.
 */
async function deployFolder(config, args, api) {
  const input = readArguments(args)
  const root = await allowedRoot(input.path, config.folders, config.host)
  const environment = await chooseEnvironment(
    api,
    config,
    input.projectId,
    input.environmentId
  )
  const collected = await collectFiles(root)
  const { zip, packed, bytes } = await packZip(collected)
  if (packed.length === 0) {
    throw new Error(
      `${root} has nothing to deploy once secrets, dependencies and what .gitignore lists are left out. ${leftOutLine(collected.leftOut)}`
    )
  }

  const boundary = `temps-${crypto.randomBytes(16).toString('hex')}`
  const body = Buffer.concat([
    Buffer.from(
      `--${boundary}\r\nContent-Disposition: form-data; name="file"; filename="source.zip"\r\nContent-Type: application/zip\r\n\r\n`
    ),
    zip,
    Buffer.from(`\r\n--${boundary}--\r\n`),
  ])
  const target = `project ${input.projectId}${environment.name ? ` (${environment.name})` : ` (environment ${environment.id})`}`
  const deployment = apiJson(
    await api(
      'POST',
      `/api/projects/${input.projectId}/environments/${environment.id}/deploy/source`,
      {
        'content-type': `multipart/form-data; boundary=${boundary}`,
        accept: 'application/json',
      },
      body
    ),
    config,
    `deploy ${target}`
  )
  const id = deployment && deployment.id !== undefined ? deployment.id : '?'
  return [
    `Deployment ${id} of ${target} started: Temps is building it now.`,
    `Uploaded ${packed.length} file${packed.length === 1 ? '' : 's'} (${megabytes(bytes)}, ${megabytes(zip.length)} zipped) from ${root}.`,
    leftOutLine(collected.leftOut),
    `Follow the build and its logs with the temps tool (deployment ${id}).`,
  ].join('\n')
}

module.exports = {
  DEPLOY_TOOL,
  DEPLOY_TOOL_NAME,
  LIMITS,
  alwaysLeftOut,
  parseIgnore,
  isIgnored,
  allowedRoot,
  collectFiles,
  packZip,
  deployFolder,
  rejectedKey,
  HOSTS,
  hostFor,
}
