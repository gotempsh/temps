# Temps TypeScript SDKs

TypeScript packages for building applications and standalone plugins on Temps.

## Packages

| Package | Purpose |
| --- | --- |
| [`@temps-sdk/plugin`](./packages/plugin-sdk) | Build TypeScript external plugins and compile them to standalone Bun executables. |
| [`@temps-sdk/node-sdk`](./packages/node-sdk) | Typed server-side client for the Temps HTTP API and error tracking. |
| [`@temps-sdk/react-analytics`](./packages/react-analytics) | React analytics, session recording, engagement, and performance tracking. |
| [`@temps-sdk/kv`](./packages/kv) | Key-value storage client. |
| [`@temps-sdk/blob`](./packages/blob) | Blob storage client. |

For a complete external plugin with a React UI, see
[`examples/node-hello-plugin`](./examples/node-hello-plugin). For a practical,
production-shaped example, see
[`examples/deployment-pulse-plugin`](./examples/deployment-pulse-plugin), a
read-only deployment health dashboard with an embedded React UI.

## React analytics

## Installation

```bash
npm install @temps-sdk/react-analytics
```

## Quick Start

```tsx
import { TempsAnalyticsProvider, useTrackEvent, useTrackPageview } from '@temps-sdk/react-analytics';

function App() {
  return (
    <TempsAnalyticsProvider>
      <YourApp />
    </TempsAnalyticsProvider>
  );
}

function YourApp() {
  const trackEvent = useTrackEvent();
  const trackPageview = useTrackPageview();

  const handleClick = () => {
    trackEvent('button_clicked', { buttonId: 'cta-button' });
  };

  return (
    <button onClick={handleClick}>
      Click me
    </button>
  );
}
```

## Configuration Options

The `TempsAnalyticsProvider` accepts the following configuration options:

### `basePath` (optional)
Base endpoint path for analytics requests. Defaults to `/api/_temps`.

```tsx
<TempsAnalyticsProvider basePath="/api/analytics">
  {children}
</TempsAnalyticsProvider>
```

### `disabled` (optional)
Set to `true` to disable analytics completely. Useful for testing or development environments.

```tsx
<TempsAnalyticsProvider disabled={process.env.NODE_ENV === 'test'}>
  {children}
</TempsAnalyticsProvider>
```

### `ignoreLocalhost` (optional)
Automatically ignore localhost and test environments. Defaults to `true`.

```tsx
<TempsAnalyticsProvider ignoreLocalhost={false}>
  {children}
</TempsAnalyticsProvider>
```

### `autoTrackPageviews` (optional)
Automatically track pageviews on route changes. Defaults to `true`.

```tsx
<TempsAnalyticsProvider autoTrackPageviews={false}>
  {children}
</TempsAnalyticsProvider>
```

### `domain` (optional)
Custom domain to use for analytics tracking. If not provided, defaults to `window.location.hostname`.

```tsx
<TempsAnalyticsProvider domain="app.example.com">
  {children}
</TempsAnalyticsProvider>
```

This is useful when you want to:
- Override the detected domain
- Use a consistent domain across different environments
- Track analytics for a specific domain regardless of the current location

## Hooks

### `useTrackEvent()`
Returns a function to track custom events.

```tsx
const trackEvent = useTrackEvent();

// Track a simple event
trackEvent('user_signup');

// Track an event with data
trackEvent('purchase_completed', {
  amount: 99.99,
  currency: 'USD',
  productId: 'prod_123'
});
```

### `useTrackPageview()`
Returns a function to manually trigger pageview tracking.

```tsx
const trackPageview = useTrackPageview();

// Manually track a pageview
trackPageview();
```

### `useTempsAnalytics()`
Returns the full analytics context with all available methods.

```tsx
const { trackEvent, trackPageview, identify, enabled } = useTempsAnalytics();
```

## Automatic Tracking

The SDK automatically tracks:
- Initial page load
- Route changes (pushState/popstate)
- Click events on elements with `temps-event-name` and `temps-data-*` attributes

### Declarative Event Tracking

You can track events directly in your JSX:

```tsx
<button
  temps-event-name="button_clicked"
  temps-data-button-id="cta-button"
  temps-data-category="conversion"
>
  Get Started
</button>
```

## Data Sent

Every analytics request includes:
- `event_name`: The name of the event
- `request_query`: Current URL query parameters
- `request_path`: Current URL path
- `domain`: Domain (configured or detected)
- `event_data`: Additional event data
- `request_id`: Request identifier (if available)
- `session_id`: Session identifier (if available)

## Development

To install dependencies:

```bash
bun install
```

To build:

```bash
bun run build
```

## License

MIT
