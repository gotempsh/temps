---
name: track-app-usage
description: Required for every app with pages - Temps visitor analytics plus named events for what visitors do, so the owner sees who comes and what gets used
requires_tool: sandbox_exec
contexts: assistant, project
---

# Track visitors and what they use

Every app you build that has pages people visit must report to Temps
analytics, and must send named events for the things visitors do in it. Page
views say someone came; events say what they used, where they stopped, and
which features nobody touches. That is what the owner needs to decide what to
build next, so this is not optional and you do not ask first.

It applies when you build an app, and when you add a page or a feature to
one: the new feature gets its events. For a narrow fix in an app that has no
analytics yet (a bug, a style, a config value), do not rewrite unrelated code;
say in your final message that it reports no usage and offer to add it.

An API, worker or bot with no pages has no visitors: say so, and skip this.

## How it reaches Temps

Temps answers `/api/_temps/*` on every deployed app's own domain, and the SDKs
send there by default. Add no route, key, `basePath`, proxy or environment
variable for it. The SDKs send nothing on `localhost` or from automated
browsers, so seeing no analytics requests in the sandbox is expected, not a
fault: check that the code builds, not that events arrive.

## Plan the events first

Before writing the pages, decide the events from what the app is for, and list
them in your plan. Aim for three to eight; not every click.

- The main action, the reason the app exists, recorded when it succeeds:
  `link_created`, `tip_sent`, `order_placed`, `booking_confirmed`.
- The steps toward it, to see where people drop off: `checkout_started`,
  `form_submitted`.
- The choices that tell the owner what people want: `plan_selected`,
  `filter_applied`, `template_chosen`.
- The main action failing as the visitor sees it: `link_create_failed` with a
  short `reason` code (`invalid_url`, `server_error`), never the error text.
- What carries value out of the app: `link_copied`, `share_clicked`,
  `outbound_clicked`.

Name events `object_verb` in the past tense, snake_case, and keep the names
stable: a value never goes in the name (`plan_selected` with `plan: 'pro'`,
not `pro_plan_selected`).

Properties describe the action with a few low-cardinality values: a plan, a
section, a count, a boolean (`source: 'hero'`, `items: 3`, `custom_code:
true`). Never send personal or user-typed data: no emails, names, phone
numbers, addresses, free text, pasted URLs, search terms, ids that identify a
person, or tokens.

Send the success event after the action succeeds (the API answered OK), not
when the button is pressed, and the `_failed` event when it does not.

## React and Next.js

`npm install @temps-sdk/react-analytics`

Wrap the app once. In Next.js App Router, `app/layout.tsx` stays a server
component:

```tsx
import { TempsAnalyticsProvider } from '@temps-sdk/react-analytics'
// …
<body>
  <TempsAnalyticsProvider>{children}</TempsAnalyticsProvider>
</body>
```

In a Vite app, wrap `<App />` in `src/main.tsx` the same way.

Send events from client components inside the provider (add `'use client'`
in Next.js):

```tsx
'use client'
import { useTrackEvent } from '@temps-sdk/react-analytics'

export function ShortenForm() {
  const track = useTrackEvent()
  async function submit(url: string) {
    const response = await fetch('/api/links', { method: 'POST', body: JSON.stringify({ url }) })
    if (response.ok) void track('link_created', { custom_code: false })
    else void track('link_create_failed', { reason: response.status >= 500 ? 'server_error' : 'invalid_url' })
  }
  // …
}
```

A plain click can be declarative instead:
`<button temps-event-name="share_clicked" temps-data-channel="copy">`.
Every `temps-data-*` attribute becomes a property.

## Vue

`npm install @temps-sdk/vue-analytics`. In `src/main.ts`,
`app.use(TempsAnalyticsPlugin)` before `app.mount`; in components,
`const track = useTrackEvent()` and `track('plan_selected', { plan })`.

## Svelte and SvelteKit

`npm install @temps-sdk/svelte-analytics`. Call `initTempsAnalytics()` once at
the root (`src/routes/+layout.svelte` in an `onMount`, or `src/main.ts`). Send
events with `getTempsAnalytics().trackEvent('plan_selected', { plan })`
inside the event handler, never at the top of a component: it throws until
`initTempsAnalytics()` has run, which is never during server rendering.

## Plain HTML and server-rendered templates

For pages with no bundler (static HTML, or templates rendered by Express,
Flask, Django, Go), add the script to every page's `<head>`, pinned to a
version:

```html
<script defer src="https://cdn.jsdelivr.net/npm/@temps-sdk/analytics-browser@0.0.8/dist/temps.min.js"></script>
```

Mark clicks with `temps-event-name` and `temps-data-*` attributes, and send
events that depend on a result from code once it is known:

```js
window.temps?.trackEvent('tip_sent', { amount_band: 'under_10' })
```

## Rules

- Do not turn on session recording unless the user asks for it.
- Check before you deploy: the build exits 0 with the SDK in, and the pages
  still render in the sandbox.
- When you finish, list each event, when it fires and its properties, and tell
  the user they appear on the project's Analytics page once the deployment
  has visitors.
