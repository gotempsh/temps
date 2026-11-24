// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

const ELEMENT_NODE = 1

/** The part of a scrolling transcript the follower reads and moves. */
export type FollowedViewport = Pick<
  HTMLElement,
  'scrollHeight' | 'scrollTop' | 'children'
>

/**
 * Keep a transcript pinned to its newest content while `shouldFollow()` is
 * true, whatever makes it grow. Rendering a message moves the view, but much
 * of a chat arrives after that: suggested next steps, deployment and sandbox
 * cards, images and code blocks all load on their own and push the end of the
 * conversation out of sight. This watches the size of every row in the
 * transcript (rows added later included) and scrolls to the bottom when the
 * content gets taller, so opening a chat lands on its latest answer and its
 * suggestions. Content that shrinks never moves the view. Returns the
 * function that stops watching.
 */
export function followTranscriptGrowth(
  viewport: FollowedViewport,
  shouldFollow: () => boolean
): () => void {
  if (
    typeof ResizeObserver === 'undefined' ||
    typeof MutationObserver === 'undefined'
  ) {
    return () => {}
  }
  let lastHeight = viewport.scrollHeight
  const follow = () => {
    const height = viewport.scrollHeight
    const grew = height > lastHeight
    lastHeight = height
    if (grew && shouldFollow()) viewport.scrollTop = height
  }
  const sizes = new ResizeObserver(follow)
  for (const row of Array.from(viewport.children)) sizes.observe(row)
  const rows = new MutationObserver((records) => {
    for (const record of records) {
      for (const node of Array.from(record.addedNodes)) {
        if (node.nodeType === ELEMENT_NODE) sizes.observe(node as Element)
      }
      for (const node of Array.from(record.removedNodes)) {
        if (node.nodeType === ELEMENT_NODE) sizes.unobserve(node as Element)
      }
    }
    follow()
  })
  rows.observe(viewport as unknown as Node, { childList: true })
  return () => {
    sizes.disconnect()
    rows.disconnect()
  }
}
