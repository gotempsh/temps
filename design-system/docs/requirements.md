# Functional requirements

Every page in the console must be rebuildable from its address alone. Not
"mostly": a reload, a pasted link, a bookmark, a screenshot with the address
bar in it — each one has to come back with the same layout, the same data and
the same reading position. This document is the requirement and the test for
it.

The reason is the reader. An operator works in tabs, sends links to the person
who can fix the thing, and reloads when the network hiccups. State that lives
only in React is state the reader cannot keep, cannot send, and loses the
moment anything goes wrong — which, on the screen where something is already
going wrong, is exactly when they had it.

Companion to `brand-guidelines.md` §6 and `design-system-handoff.md` §7.
Reference implementation: `ConsoleV1Logs.tsx` (`?q=` `?cols=` `?lv=`) and the
shared hook in `src/sections/console-url.ts`.

## The URL is the state

The path names the record. The query names the view of it.

- Path: which thing this page is about — the project, the deployment, the
  database, the issue, the node. One record, one address.
- Query: everything that changes what is on screen for that record — the tab
  or facet (`tab`), the filter text (`f`), the sort (`sort`), the page
  (`page`), the time range (`range`), the inspected row (`p=log:…`), the
  chosen columns (`cols`), the rendering (`lv`).

Nothing that changes what the reader sees lives only in `useState`. If two
readers on the same address would see different screens, the address is
incomplete and the page is broken.

Omit defaults. `?tab=overview` on a record whose first facet is overview adds
a parameter and says nothing; the absence of the key *is* the default. A URL
carries the deltas from the default view, so the common address stays short
enough to read aloud and a screen with nothing chosen has no query at all.

Replace for a view change, push for a navigation. Retyping a filter must not
fill the history with a keystroke per character, and `back` after switching a
tab six times must not be six presses. Going somewhere pushes; looking at the
same thing differently replaces.

## Reload is loss-free

A reload rebuilds the same layout and the same data. So does a link pasted
into another tab, another browser, another person's machine.

The test is a **reload signature**: the document title, the page heading, the
active tab or facet, the first three row ids, and the range label. Compute it,
reload, compute it again. The two strings are identical or the page has a bug.

It is a signature, not a screenshot, on purpose: it names the things a reader
would notice were gone. A page that comes back on the right record but on the
wrong facet, or with the filter cleared, or on page 1 of a list they were on
page 4 of, has lost their place, and losing a reader's place is the same
failure whether it looks like a crash or not.

## Fetching follows the URL

Every fetch is a function of the path and the parameters, and of nothing else.

- Derive the request from the address, then run it. Never from a value that
  only exists because a component happens to be mounted.
- The same URL fetches the same thing. Two loads of one address ask the server
  the same question, in the same order, with the same window.
- A parameter the fetch depends on is a parameter in the URL. If narrowing a
  list changes the request, the narrowing is in the query.
- Read it once, at the top of the screen, and pass values down. A component
  deep in the tree that reaches for the address again is a second source of
  truth waiting to disagree with the first.

## Links are complete

Every link the console emits carries the whole view, not the bare record.

- A row that opens a record links to the record with the reader's range still
  on it, so the chart on the other side covers the window they were reading.
- `copy link` copies the address you are on — the full address, parameters and
  all. A copy button that hands over a link that opens a different screen is
  worse than no copy button.
- `back` and `forward` walk views the reader actually visited, in order.
- An `open in …` action ("open in Logs", "show the trace") writes its narrowing
  into the query of the page it opens, so the reader lands on the answer and
  can widen from there.

## Layout is independent of data timing

The shape of the page comes from the address; only the values come from the
network.

- Draw the skeleton from the URL: the same tabs, the same columns, the same
  number of rows the page will hold. `PageState state="loading"` holds the
  shape.
- Nothing reflows when the data lands. A page that jumps has told the reader
  their eye was in the wrong place.
- The page's state is addressable too. Empty, unconfigured, error and
  not-found are views of an address, reachable by loading it — never a
  condition you can only get to by clicking through the happy path first.

## Errors are addressable

An error page has a URL you can send to somebody.

- A failed load keeps the address that failed. The retry re-runs it; it does
  not navigate away and it does not clear the query.
- The error names what failed, on what, why, and what to do next, with the id
  — and the address still names the record, so the person you sent it to opens
  the same failure and not a fresh dashboard.
- A "not set up" state is addressable in the same way: the link goes to the
  page, the page says what is missing and links the setting that fixes it.

## What stays local

Not everything is view state. These belong to the moment, not the address:

- A hover, a focus ring, a ledger cursor that has not opened anything.
- An open menu, drop, popover or tooltip. A dialog that asks a question is a
  moment, not a place.
- A transient answer on a control: `copied`, `saving…`, `noted`.
- A draft the reader has not submitted — the text in a form, the query being
  typed before it is committed.

The rule for a draft: if losing it on reload would cost the reader work, the
page says so before it is lost. Either keep it (the sticky save bar is dirty
and stays dirty) or warn on the way out. Silently discarding typing is the one
way a local-only state is allowed to hurt, and it is not allowed.

## One full address

```
/v1?p=api-gateway&tab=deploys&f=failed&sort=started&page=2&range=7d
     │            │           │        │            │      └ time window: 7d
     │            │           │        │            └ page 2 of the ledger
     │            │           │        └ sorted by start time
     │            │           └ filter text: failed
     │            └ the deploys facet of the record
     └ the record: project api-gateway
```

Six keys, and every one of them changes what is on screen. Drop any of them
and the reader lands somewhere else. Add `?tab=overview` and you have added
nothing, which is why the default is omitted.

## How to test

1. Load the address. Read the signature: title · heading · active facet ·
   first three row ids · range label.
2. Reload. The signature is identical.
3. Change a filter, a tab, a sort, a page and a range in the UI. `location.search`
   changes each time, and names what you changed.
4. Copy the address out, open it in a second tab. Same screen.
5. Press `back`. You are on the view before, not on a keystroke of a filter.
6. Copy a link from a row and from `copy link`. Both open the view you were
   looking at.
7. Load the error and the empty address directly. Both render.
8. Reload with something typed and unsubmitted. Either it survives or the page
   told you it would not.
