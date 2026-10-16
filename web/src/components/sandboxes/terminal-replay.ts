// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Reattaching to a terminal replays the tail of its output (the server keeps
 * 64 KiB) as the first message of the new socket. The xterm instance outlives
 * the socket and still holds everything it was shown, often far more than the
 * replay window, so clearing it first throws history away and writing the
 * replay on top of it repeats what is already on screen.
 *
 * The replay is the end of the same byte stream the terminal has already
 * consumed, so the bytes the terminal has seen last locate where the replay
 * stops repeating and starts carrying what was produced while disconnected.
 */

/** How many trailing bytes of the stream are kept to find that point. */
export const REPLAY_ANCHOR_BYTES = 256

/**
 * Below this there is too little history to anchor on reliably, and too little
 * on screen to be worth preserving.
 */
export const MIN_REPLAY_ANCHOR_BYTES = 32

/** The last `limit` bytes of `tail` followed by `bytes`. */
export function appendOutputTail(
  tail: Uint8Array,
  bytes: Uint8Array,
  limit: number = REPLAY_ANCHOR_BYTES
): Uint8Array {
  if (bytes.length >= limit) return bytes.slice(bytes.length - limit)
  const keep = Math.min(tail.length, limit - bytes.length)
  const next = new Uint8Array(keep + bytes.length)
  next.set(tail.subarray(tail.length - keep), 0)
  next.set(bytes, keep)
  return next
}

function indexOfBytes(haystack: Uint8Array, needle: Uint8Array): number {
  const last = haystack.length - needle.length
  const first = needle[0]
  for (let start = 0; start <= last; start += 1) {
    if (haystack[start] !== first) continue
    let offset = 1
    while (
      offset < needle.length &&
      haystack[start + offset] === needle[offset]
    ) {
      offset += 1
    }
    if (offset === needle.length) return start
  }
  return -1
}

export interface ReplayResync {
  /** The terminal's own history cannot be continued; clear it first. */
  reset: boolean
  /** What to write to the terminal. */
  bytes: Uint8Array
}

/**
 * Decide how to apply the first output of a new attach.
 *
 * `seen` is the end of the stream the terminal already consumed. When it is
 * found in `incoming`, only what follows it is new and the terminal keeps its
 * history. Otherwise (a gap longer than the replay window, or a different
 * shell) there is nothing to continue and the terminal starts over from the
 * replay.
 *
 * The first match is used, so a repetitive stream can only make a little
 * already-seen output show twice, never drop output.
 */
export function resyncReplay(
  seen: Uint8Array,
  incoming: Uint8Array
): ReplayResync {
  if (seen.length < MIN_REPLAY_ANCHOR_BYTES) {
    return { reset: true, bytes: incoming }
  }
  const at = indexOfBytes(incoming, seen)
  if (at < 0) return { reset: true, bytes: incoming }
  return { reset: false, bytes: incoming.subarray(at + seen.length) }
}
