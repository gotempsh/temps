# @temps-sdk/analytics-browser

Vanilla JS analytics SDK for Temps. Use it programmatically or as a zero-config CDN script.

## Installation

```bash
npm install @temps-sdk/analytics-browser
```

## Programmatic

```ts
import { init, trackEvent } from "@temps-sdk/analytics-browser";

init({
  basePath: "/api/_temps",
  domain: "example.com",
  enableSessionRecording: false,
});

// Later in your app:
trackEvent("signup", { plan: "pro" });
```

After `init()`, a global `window.temps` handle is exposed for convenience.

## CDN script tag (zero config)

```html
<script
  defer
  src="https://cdn.jsdelivr.net/npm/@temps-sdk/analytics-browser@0.0.1/dist/temps.min.js"
></script>
```

By default, the script posts to the current site's relative `/api/_temps/*`
endpoints. The script CDN origin is never used for ingest requests.

All `data-*` attributes on the script tag map to `AnalyticsOptions`
(kebab-case to camelCase), and are only needed for overrides.

```html
<script
  defer
  src="https://cdn.jsdelivr.net/npm/@temps-sdk/analytics-browser@0.0.1/dist/temps.min.js"
  data-session-recording="true"
></script>
```

For apps deployed on Temps, `/api/_temps/*` is handled by the platform. For
external apps, add a same-origin rewrite or proxy for `/api/_temps/*` to the
Temps instance.

### Attributes

| Attribute | Type | Notes |
|---|---|---|
| `data-domain` | string | Overrides detected hostname |
| `data-base-path` | string | Optional override. Defaults to `/api/_temps` |
| `data-disabled` | `"true"` \| `"false"` | Kill switch |
| `data-ignore-localhost` | `"true"` \| `"false"` | Default: true |
| `data-auto-track-pageviews` | `"true"` \| `"false"` | Default: true |
| `data-auto-track-page-leave` | `"true"` \| `"false"` | Default: true |
| `data-auto-track-speed-analytics` | `"true"` \| `"false"` | Default: true |
| `data-auto-track-engagement` | `"true"` \| `"false"` | Default: true |
| `data-heartbeat-interval` | number (ms) | Default: 30000 |
| `data-session-recording` | `"true"` \| `"false"` | Default: false |
| `data-session-sample-rate` | 0.0 - 1.0 | Default: 1.0 |
| `data-excluded-paths` | csv | e.g. `/admin/*,/settings/*` |

## HTML attributes for declarative events

```html
<button temps-event-name="cta_click" temps-data-section="hero">Get started</button>
```

Any element with `temps-event-name` fires an event on click. All `temps-data-*`
attributes are sent as event properties.

## API

```ts
import { init, getAnalytics, trackEvent, trackPageview, destroy } from "@temps-sdk/analytics-browser";
import { createAnalytics } from "@temps-sdk/analytics-browser"; // re-exported from core
```

`init()` replaces any previous instance (useful for hot reload); `destroy()` tears it down.

## Publishing

`bun run build` emits:

- `dist/index.js` and `dist/auto.js` for ESM imports
- `dist/temps.js` and `dist/temps.min.js` for CDN script tags

Before publishing, run:

```bash
bun run test:run
npm publish --access public
```
