// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Where a secret value comes from: a file, stdin, or (for compatibility) the
 * command line. A value on the command line ends up in shell history, `ps`
 * output and CI logs, so every command that takes a secret or credential
 * offers `--…-file <path>` and `--…-stdin` through this module.
 *
 * The value itself is never printed: warnings and errors name the key, the
 * flag and the file path, never the content. That includes the legacy
 * `-v @path` form, whose "path" may really be a secret that starts with `@`.
 */

import type { Command } from 'commander'
import { constants } from 'node:fs'
import { open } from 'node:fs/promises'

/**
 * Largest value accepted from any source, in bytes. Project secrets are capped
 * at exactly this size by the API (`SECRET_VALUE_MAX_BYTES` in
 * `crates/temps-environments/src/services/secret_service.rs`). Environment
 * variables, agent secrets and provider credentials are sent as JSON strings
 * under the server's 2 MiB default request-body limit; JSON escaping can
 * double a value's size, so 1 MiB is the most that always fits there too.
 */
export const MAX_VALUE_BYTES = 1_048_576

/** How the value was supplied; safe to print, unlike the value. */
export type ValueSourceKind = 'argument' | 'file' | 'stdin' | 'legacy-file'

export interface ResolvedValue {
  value: string
  source: ValueSourceKind
}

/** What the user passed, as Commander parsed it. */
export interface ValueSourceInput {
  /** The positional argument or `-v/--value` option. */
  inline?: string
  /** `--value-file <path>`; `-` reads stdin. */
  file?: string
  /** `--value-stdin`. */
  stdin?: boolean
  /** `--literal`: send an inline value starting with `@` as is. */
  literal?: boolean
  /** `--trim-trailing-newline`: drop one trailing `\n` or `\r\n`. */
  trimTrailingNewline?: boolean
}

export interface ValueSourceSpec {
  /** Names the value in messages, e.g. `the value for DB_PASSWORD`. Never the value itself. */
  subject: string
  /** How each source is spelled on this command, for conflict and missing-value errors. */
  flags: { inline: string; file: string; stdin: string }
  /** Fail when no source is given (otherwise resolve to `undefined`). */
  required?: boolean
  /**
   * Read an inline value starting with `@` as a file path, with a deprecation
   * note (`secrets create -v @./creds.json`). Only for commands that always did.
   */
  legacyAtPath?: boolean
  /**
   * Hint when a value read from a file or stdin ends in a newline. Right for
   * single-line values (env vars, tokens, passwords), where it is almost
   * always an accident of `echo` or an editor; noise for PEM files.
   */
  newlineHint?: boolean
  /**
   * Refuse an empty value read from a file or stdin (after any trimming).
   * Right for credentials: an empty token or signing secret is never meant,
   * and some commands would quietly treat it as "none".
   */
  rejectEmpty?: boolean
  maxBytes?: number
}

/** Minimal stdin shape, so tests can feed a pipe or pretend to be a terminal. */
export interface StdinLike {
  isTTY?: boolean
  [Symbol.asyncIterator](): AsyncIterator<Uint8Array | string>
}

export interface ValueSourceIO {
  stdin: StdinLike
  /** Writes one line to stderr. */
  warn: (message: string) => void
  platform: NodeJS.Platform
}

const defaultIO = (): ValueSourceIO => ({
  stdin: process.stdin,
  warn: (message) => process.stderr.write(`${message}\n`),
  platform: process.platform,
})

/** A problem with where the value comes from. Its message never contains the value. */
export class ValueSourceError extends Error {
  constructor(message: string) {
    super(message)
    this.name = 'ValueSourceError'
  }
}

function formatBytes(bytes: number): string {
  return bytes % 1_048_576 === 0 ? `${bytes / 1_048_576} MiB` : `${bytes} bytes`
}

function tooLarge(subject: string, where: string, maxBytes: number): ValueSourceError {
  return new ValueSourceError(`${subject} from ${where} is larger than the ${formatBytes(maxBytes)} limit the API accepts`)
}

function decodeUtf8(bytes: Uint8Array, subject: string, where: string): string {
  try {
    // fatal: report invalid bytes instead of replacing them; ignoreBOM: keep
    // a byte-order mark rather than silently dropping it.
    return new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(bytes)
  } catch {
    throw new ValueSourceError(
      `${subject} from ${where} is not valid UTF-8 text. The API stores values as text and does not decode ` +
        'base64, so binary content cannot be sent as is: encode it yourself (e.g. `base64 -i <file>`) and decode it where it is used.',
    )
  }
}

function errorCode(err: unknown): string {
  return typeof err === 'object' && err !== null && 'code' in err ? String((err as { code: unknown }).code) : 'unknown error'
}

/**
 * Read a regular file, refusing directories, FIFOs, sockets and devices, and
 * anything over `maxBytes`. `display` names the file in messages: the path for
 * `--value-file`, a neutral phrase for the legacy form.
 */
async function readValueFile(
  path: string,
  display: string,
  spec: ValueSourceSpec,
  maxBytes: number,
  io: ValueSourceIO,
): Promise<string> {
  // Open without blocking, then check the opened descriptor rather than the
  // path, so a file swapped for a FIFO after a check cannot hang the read.
  // O_NONBLOCK does not change reads of a regular file; Windows has no FIFOs.
  const flags = constants.O_RDONLY | (constants.O_NONBLOCK ?? 0)
  let handle
  try {
    handle = await open(path, flags)
  } catch (err) {
    if (errorCode(err) === 'EISDIR') {
      throw new ValueSourceError(`Cannot read ${spec.subject} from ${display}: it is a directory, not a file`)
    }
    throw new ValueSourceError(`Cannot read ${spec.subject} from ${display}: ${errorCode(err)}`)
  }
  try {
    let info
    try {
      info = await handle.stat()
    } catch (err) {
      throw new ValueSourceError(`Cannot read ${spec.subject} from ${display}: ${errorCode(err)}`)
    }
    if (info.isDirectory()) {
      throw new ValueSourceError(`Cannot read ${spec.subject} from ${display}: it is a directory, not a file`)
    }
    if (!info.isFile()) {
      throw new ValueSourceError(
        `Cannot read ${spec.subject} from ${display}: it is not a regular file (a pipe, socket or device). ` +
          `To pipe the value in, use ${spec.flags.stdin}.`,
      )
    }
    if (info.size > maxBytes) throw tooLarge(spec.subject, display, maxBytes)
    if (io.platform !== 'win32' && (info.mode & 0o044) !== 0) {
      const mode = (info.mode & 0o777).toString(8).padStart(3, '0')
      io.warn(
        `warning: ${display} is readable by other users (mode ${mode}); consider \`chmod 600\` on it`,
      )
    }

    // Read at most one byte past the cap, in case the file grew since stat.
    const buffer = new Uint8Array(maxBytes + 1)
    let length = 0
    while (length < buffer.length) {
      const { bytesRead } = await handle.read(buffer, length, buffer.length - length, null)
      if (bytesRead === 0) break
      length += bytesRead
    }
    if (length > maxBytes) throw tooLarge(spec.subject, display, maxBytes)
    return decodeUtf8(buffer.subarray(0, length), spec.subject, display)
  } catch (err) {
    if (err instanceof ValueSourceError) throw err
    throw new ValueSourceError(`Cannot read ${spec.subject} from ${display}: ${errorCode(err)}`)
  } finally {
    await handle.close()
  }
}

async function readValueStdin(spec: ValueSourceSpec, flag: string, maxBytes: number, io: ValueSourceIO): Promise<string> {
  if (io.stdin.isTTY) {
    throw new ValueSourceError(
      `${flag} reads ${spec.subject} from stdin, but stdin is a terminal with nothing piped in. ` +
        `Pipe it in (e.g. \`op read op://vault/item/field | temps … ${flag}\`) or use ${spec.flags.file} <path>.`,
    )
  }
  const chunks: Uint8Array[] = []
  let length = 0
  for await (const chunk of io.stdin) {
    const bytes = typeof chunk === 'string' ? new TextEncoder().encode(chunk) : chunk
    length += bytes.length
    if (length > maxBytes) throw tooLarge(spec.subject, 'stdin', maxBytes)
    chunks.push(bytes)
  }
  if (length === 0) {
    throw new ValueSourceError(`${flag} was given but nothing was piped on stdin for ${spec.subject}`)
  }
  const bytes = new Uint8Array(length)
  let offset = 0
  for (const chunk of chunks) {
    bytes.set(chunk, offset)
    offset += chunk.length
  }
  return decodeUtf8(bytes, spec.subject, 'stdin')
}

/** Drop exactly one trailing `\r\n` or `\n`. */
export function trimOneTrailingNewline(value: string): string {
  if (value.endsWith('\r\n')) return value.slice(0, -2)
  if (value.endsWith('\n')) return value.slice(0, -1)
  return value
}

/**
 * Resolve a value from exactly one source. Throws a `ValueSourceError` naming
 * the flags when more than one is given, or none when `spec.required`.
 * The bytes are kept exactly as read unless `trimTrailingNewline` is set.
 */
export async function resolveValueSource(
  input: ValueSourceInput,
  spec: ValueSourceSpec,
  io: ValueSourceIO = defaultIO(),
): Promise<ResolvedValue | undefined> {
  const maxBytes = spec.maxBytes ?? MAX_VALUE_BYTES
  const fileIsStdin = input.file === '-'

  const given: string[] = []
  if (input.inline !== undefined) given.push(spec.flags.inline)
  if (input.file !== undefined) given.push(fileIsStdin ? `${spec.flags.file} -` : spec.flags.file)
  if (input.stdin) given.push(spec.flags.stdin)
  if (given.length > 1) {
    throw new ValueSourceError(`Pass ${spec.subject} from only one source, got ${given.join(' and ')}`)
  }
  if (input.file === '') {
    throw new ValueSourceError(`${spec.flags.file} needs a path (or - for stdin)`)
  }

  let resolved: ResolvedValue | undefined
  if (input.stdin || fileIsStdin) {
    const flag = input.stdin ? spec.flags.stdin : `${spec.flags.file} -`
    resolved = { value: await readValueStdin(spec, flag, maxBytes, io), source: 'stdin' }
  } else if (input.file !== undefined) {
    resolved = { value: await readValueFile(input.file, input.file, spec, maxBytes, io), source: 'file' }
  } else if (input.inline !== undefined) {
    if (spec.legacyAtPath && input.inline.startsWith('@') && !input.literal) {
      io.warn(
        `note: reading ${spec.flags.inline} @<path> as a file is deprecated; use ${spec.flags.file} <path> ` +
          '(or --literal to send a value that starts with @)',
      )
      // Never echo the path: it may be a real secret that starts with "@".
      const display = `the file named by ${spec.flags.inline} @… (pass --literal if the value itself starts with @)`
      resolved = { value: await readValueFile(input.inline.slice(1), display, spec, maxBytes, io), source: 'legacy-file' }
    } else {
      if (Buffer.byteLength(input.inline, 'utf8') > maxBytes) throw tooLarge(spec.subject, spec.flags.inline, maxBytes)
      resolved = { value: input.inline, source: 'argument' }
    }
  }

  if (resolved === undefined) {
    if (spec.required) {
      throw new ValueSourceError(
        `Missing ${spec.subject}: pass ${spec.flags.file} <path>, ${spec.flags.stdin}, or ${spec.flags.inline}`,
      )
    }
    return undefined
  }

  if (input.trimTrailingNewline) {
    resolved.value = trimOneTrailingNewline(resolved.value)
  }
  if (spec.rejectEmpty && resolved.source !== 'argument' && resolved.value === '') {
    const where = resolved.source === 'stdin' ? 'stdin' : 'the file'
    throw new ValueSourceError(`${spec.subject} read from ${where} is empty${input.trimTrailingNewline ? ' after trimming the trailing newline' : ''}`)
  }
  if (!input.trimTrailingNewline && spec.newlineHint && resolved.source !== 'argument' && resolved.value.endsWith('\n')) {
    io.warn(
      `hint: ${spec.subject} read from ${resolved.source === 'stdin' ? 'stdin' : 'the file'} ends with a newline, ` +
        'which is kept; pass --trim-trailing-newline to drop it',
    )
  }
  return resolved
}

// --- Commander wiring ---

const TRIM_FLAG = '--trim-trailing-newline'
const TRIM_DESCRIPTION = 'Drop exactly one trailing newline (\\n or \\r\\n) from a value read from a file or stdin (kept byte-for-byte by default)'

function addTrimOption(cmd: Command): void {
  if (!cmd.options.some((option) => option.long === TRIM_FLAG)) {
    cmd.option(TRIM_FLAG, TRIM_DESCRIPTION)
  }
}

/**
 * Add `--value-file`, `--value-stdin` and `--trim-trailing-newline` (and
 * `--literal` for commands that accept the legacy `@path` form). The command
 * reads them back with `valueSourceInput`.
 */
export function addValueSourceOptions(cmd: Command, options: { what: string; legacyAtPath?: boolean }): Command {
  cmd
    .option('--value-file <path>', `Read ${options.what} from a file, byte-for-byte (- reads stdin). Keeps it out of shell history`)
    .option('--value-stdin', `Read ${options.what} from stdin, e.g. piped from a secret manager`)
  if (options.legacyAtPath) {
    cmd.option('--literal', 'Send a -v value that starts with @ as is, instead of reading it as a file path (deprecated form)')
  }
  addTrimOption(cmd)
  return cmd
}

/** The `ValueSourceInput` from options registered by `addValueSourceOptions`. */
export function valueSourceInput(inline: string | undefined, options: Record<string, unknown>): ValueSourceInput {
  return {
    inline,
    file: options.valueFile as string | undefined,
    stdin: options.valueStdin === true,
    literal: options.literal === true,
    trimTrailingNewline: options.trimTrailingNewline === true,
  }
}

/** A credential option such as `--token <token>`, given file and stdin alternatives. */
export interface CredentialOption {
  /** Long flag without dashes, e.g. `token`, `secret-access-key`. */
  flag: string
  /** Names it in messages, e.g. `the access token`. */
  what: string
}

function camelCase(flag: string): string {
  return flag.replace(/-([a-z])/g, (_, letter: string) => letter.toUpperCase())
}

/**
 * Give credential options `--<flag>-file <path>` and `--<flag>-stdin`
 * alternatives. A pre-action hook reads the chosen source into the original
 * option, so the action sees `options.<flag>` as before and needs no changes.
 * At most one credential per command can come from stdin.
 */
export function addCredentialSources(
  cmd: Command,
  credentials: CredentialOption[],
  io: () => ValueSourceIO = defaultIO,
): Command {
  for (const credential of credentials) {
    cmd
      .option(`--${credential.flag}-file <path>`, `Read ${credential.what} from a file (- reads stdin) instead of --${credential.flag}`)
      .option(`--${credential.flag}-stdin`, `Read ${credential.what} from stdin instead of --${credential.flag}`)
  }
  addTrimOption(cmd)

  cmd.hook('preAction', async (thisCommand) => {
    const opts = thisCommand.opts() as Record<string, unknown>
    const fromStdin = credentials
      .filter((c) => opts[`${camelCase(c.flag)}Stdin`] === true || opts[`${camelCase(c.flag)}File`] === '-')
      .map((c) => (opts[`${camelCase(c.flag)}Stdin`] === true ? `--${c.flag}-stdin` : `--${c.flag}-file -`))
    if (fromStdin.length > 1) {
      throw new ValueSourceError(`Only one value can be read from stdin per command, got ${fromStdin.join(' and ')}`)
    }
    for (const credential of credentials) {
      const key = camelCase(credential.flag)
      const resolved = await resolveValueSource(
        {
          inline: opts[key] as string | undefined,
          file: opts[`${key}File`] as string | undefined,
          stdin: opts[`${key}Stdin`] === true,
          trimTrailingNewline: opts.trimTrailingNewline === true,
        },
        {
          subject: credential.what,
          flags: {
            inline: `--${credential.flag}`,
            file: `--${credential.flag}-file`,
            stdin: `--${credential.flag}-stdin`,
          },
          newlineHint: true,
          rejectEmpty: true,
        },
        io(),
      )
      if (resolved !== undefined) thisCommand.setOptionValue(key, resolved.value)
    }
  })
  return cmd
}
