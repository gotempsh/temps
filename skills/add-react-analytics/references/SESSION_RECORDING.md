# Session Recording

Privacy-aware session recording (rrweb under the hood) with visual replay in the Temps dashboard.

> **Verified against the real package.** There are **two distinct ways** to record, with different APIs. A prior version of this doc invented `<SessionRecordingProvider enabled maskAllInputs blockClass>` and a `/api/_temps/recordings` endpoint — both wrong.

## Recommended: configure recording on the analytics provider

Recording is driven by the **main** `TempsAnalyticsProvider`. This is the default path — no extra provider needed.

```tsx
import { TempsAnalyticsProvider } from '@temps-sdk/react-analytics';

<TempsAnalyticsProvider
  basePath="/api/_temps"
  enableSessionRecording={true}
  sessionRecordingConfig={{
    maskAllInputs: true,        // default true
    sessionSampleRate: 1.0,     // 0.0–1.0, default 1.0
    excludedPaths: ['/admin'],  // paths never recorded
    blockClass: 'rr-block',     // CSS class to block (default 'rr-block')
    maskTextClass: 'rr-mask',   // CSS class to mask text (default 'rr-mask')
    ignoreClass: 'rr-ignore',   // CSS class to ignore (default 'rr-ignore')
    recordCanvas: false,        // default false
    collectFonts: false,        // default false
    batchSize: 100,             // events per flush (provider-level default 100)
    flushInterval: 5000,        // ms between flushes (provider-level default 5000)
  }}
>
  {children}
</TempsAnalyticsProvider>
```

`sessionRecordingConfig` is the `TempsAnalyticsProviderProps["sessionRecordingConfig"]` shape — confirm the full field list in `package/dist/types.d.ts`.

Recording events POST to `${basePath}/session-replay` (i.e. `/api/_temps/session-replay` for Temps-hosted apps), captured by the `temps-analytics-session-replay` backend.

## Optional: user-toggleable recording (consent flows)

For a user-facing on/off toggle, use the separate `SessionRecordingProvider` and its control hooks. **Its props and hook shape are different from the config object above.**

```ts
// Real signatures from the package:
function SessionRecordingProvider(props: {
  children: React.ReactNode;
  defaultEnabled?: boolean;       // initial state
  persistPreference?: boolean;    // remember the user's choice (localStorage)
}): JSX.Element;

function useSessionRecording(): {
  isRecordingEnabled: boolean;
  enableRecording: () => void;
  disableRecording: () => void;
  toggleRecording: () => void;
  sessionId: string | null;
};

function useSessionRecordingControl(defaultEnabled?: boolean): {
  isEnabled: boolean;
  enable: () => void;
  disable: () => void;
  toggle: () => void;
};
```

> ⚠️ `SessionRecordingProvider` does **not** accept `enabled`, `maskAllInputs`, `blockClass`, or `sampling`. Masking/blocking is configured via `sessionRecordingConfig` on the analytics provider (above). The control hook returns `{ isEnabled, enable, disable, toggle }` — **not** `{ isRecording, startRecording, stopRecording }`.

### Setup

```tsx
'use client';
import { SessionRecordingProvider } from '@temps-sdk/react-analytics';

export function RecordingToggleRoot({ children }: { children: React.ReactNode }) {
  return (
    <SessionRecordingProvider defaultEnabled={false} persistPreference={true}>
      {children}
    </SessionRecordingProvider>
  );
}
```

### Control component

```tsx
'use client';
import { useSessionRecordingControl } from '@temps-sdk/react-analytics';

function RecordingControls() {
  const { isEnabled, enable, disable, toggle } = useSessionRecordingControl();
  return (
    <div>
      <span>Recording: {isEnabled ? 'On' : 'Off'}</span>
      <button onClick={toggle}>{isEnabled ? 'Stop' : 'Start'}</button>
    </div>
  );
}
```

### GDPR consent banner

```tsx
'use client';
import { useSessionRecordingControl } from '@temps-sdk/react-analytics';
import { useState, useEffect } from 'react';

function ConsentBanner() {
  const [show, setShow] = useState(false);
  const { enable, disable } = useSessionRecordingControl();

  useEffect(() => {
    const consent = localStorage.getItem('session_recording_consent');
    if (consent === null) setShow(true);
    else if (consent === 'true') enable();
  }, [enable]);

  if (!show) return null;
  return (
    <div className="fixed bottom-4 right-4 rounded bg-white p-4 shadow-lg">
      <p>We record sessions to improve your experience.</p>
      <div className="mt-2 flex gap-2">
        <button onClick={() => { localStorage.setItem('session_recording_consent', 'true'); enable(); setShow(false); }}>Accept</button>
        <button onClick={() => { localStorage.setItem('session_recording_consent', 'false'); disable(); setShow(false); }}>Decline</button>
      </div>
    </div>
  );
}
```

## Privacy controls (masking & blocking)

Masking/blocking is driven by the CSS classes you configure in `sessionRecordingConfig` (defaults: `rr-block`, `rr-mask`, `rr-ignore`).

```tsx
// Block an element entirely (replaced by a placeholder in replay)
<div className="rr-block"><CreditCardForm /></div>

// Mask text content (shown as asterisks in replay)
<span className="rr-mask">{accountBalance}</span>

// Ignore an element from recording
<div className="rr-ignore"><NoisyWidget /></div>
```

If you set custom class names in `sessionRecordingConfig`, use those instead.

## Verification

1. Open DevTools → Network and look for POSTs to `/api/_temps/session-replay`.
2. Interact with the app to generate events.
3. Open the session replay in the Temps dashboard.
4. Confirm masked/blocked elements are obscured in the replay.

## Troubleshooting

- **Nothing recorded:** confirm `enableSessionRecording={true}` on the provider (or that the user enabled it via the toggle), and that `ignoreLocalhost` isn't suppressing all traffic on localhost.
- **Sensitive data visible:** add the configured block/mask class to the element, and keep `maskAllInputs: true`.
- **Too many requests:** raise `sessionSampleRate` granularity, `batchSize`, or `flushInterval`.
