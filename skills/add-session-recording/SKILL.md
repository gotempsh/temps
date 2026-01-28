---
name: add-session-recording
description: |
  Add privacy-aware session recording and replay to React applications using Temps SDK. Captures user interactions for playback while respecting privacy through input masking, element blocking, and GDPR-compliant consent flows. Use when the user wants to: (1) Add session recording to their app, (2) Implement session replay functionality, (3) Record user sessions for debugging, (4) Add privacy-compliant screen recording, (5) Debug user issues with visual replay, (6) Implement rrweb-based recording, (7) Set up GDPR-compliant session capture. Triggers: "session recording", "session replay", "record sessions", "user replay", "screen recording", "rrweb", "session capture".
---

# Add Session Recording

Implement privacy-aware session recording with Temps SDK using rrweb under the hood.

## Installation

```bash
npm install @temps-sdk/react-analytics
```

## Quick Setup

```tsx
// app/providers.tsx or app/layout.tsx
'use client';

import {
  TempsAnalyticsProvider,
  SessionRecordingProvider
} from '@temps-sdk/react-analytics';

export function Providers({ children }) {
  return (
    <TempsAnalyticsProvider basePath="/api/_temps">
      <SessionRecordingProvider
        enabled={true}
        maskAllInputs={true}
        blockClass="sensitive"
      >
        {children}
      </SessionRecordingProvider>
    </TempsAnalyticsProvider>
  );
}
```

## Provider Options

```tsx
<SessionRecordingProvider
  enabled={true}              // Enable recording
  maskAllInputs={true}        // Mask all input values (recommended)
  maskAllText={false}         // Mask all text content
  blockClass="sensitive"      // CSS class to block elements
  ignoreClass="no-record"     // CSS class to ignore elements
  sampling={{
    mousemove: true,
    mouseInteraction: true,
    scroll: true,
    input: 'last',            // 'all' | 'last' | false
  }}
>
  {children}
</SessionRecordingProvider>
```

## Control Recording Programmatically

```tsx
'use client';

import { useSessionRecordingControl } from '@temps-sdk/react-analytics';

function RecordingControls() {
  const {
    isRecording,
    startRecording,
    stopRecording,
    toggleRecording
  } = useSessionRecordingControl();

  return (
    <div>
      <span>Recording: {isRecording ? 'Active' : 'Paused'}</span>
      <button onClick={toggleRecording}>
        {isRecording ? 'Stop' : 'Start'} Recording
      </button>
    </div>
  );
}
```

## Privacy Controls

### Block Sensitive Content

```tsx
// Method 1: CSS class (configured in provider)
<div className="sensitive">
  <CreditCardForm />
</div>

// Method 2: Data attribute
<input type="password" data-rr-block />

// Method 3: Mask text (shows asterisks in replay)
<span data-rr-mask>{socialSecurityNumber}</span>
```

### Common Patterns

```tsx
// Payment forms - block entirely
<form className="sensitive">
  <input name="card" />
  <input name="cvv" />
</form>

// Personal data - mask individual fields
<input name="ssn" data-rr-block />
<input name="dob" data-rr-mask />

// Entire sections
<section data-rr-block>
  <MedicalRecords />
</section>
```

## GDPR Consent Flow

```tsx
'use client';

import { useSessionRecordingControl } from '@temps-sdk/react-analytics';
import { useState, useEffect } from 'react';

function ConsentBanner() {
  const [showBanner, setShowBanner] = useState(false);
  const { startRecording, stopRecording } = useSessionRecordingControl();

  useEffect(() => {
    const consent = localStorage.getItem('session_recording_consent');
    if (consent === null) {
      setShowBanner(true);
    } else if (consent === 'true') {
      startRecording();
    }
  }, []);

  const handleAccept = () => {
    localStorage.setItem('session_recording_consent', 'true');
    startRecording();
    setShowBanner(false);
  };

  const handleDecline = () => {
    localStorage.setItem('session_recording_consent', 'false');
    stopRecording();
    setShowBanner(false);
  };

  if (!showBanner) return null;

  return (
    <div className="fixed bottom-4 right-4 p-4 bg-white shadow-lg rounded">
      <p>We record sessions to improve your experience.</p>
      <div className="flex gap-2 mt-2">
        <button onClick={handleAccept}>Accept</button>
        <button onClick={handleDecline}>Decline</button>
      </div>
    </div>
  );
}
```

## Conditional Recording

```tsx
// Only record in production
<SessionRecordingProvider
  enabled={process.env.NODE_ENV === 'production'}
>

// Only record for specific users
<SessionRecordingProvider
  enabled={user?.plan === 'enterprise'}
>

// Disable for specific pages
function CheckoutPage() {
  const { stopRecording, startRecording } = useSessionRecordingControl();

  useEffect(() => {
    stopRecording();
    return () => startRecording();
  }, []);

  return <CheckoutForm />;
}
```

## Verification

1. Open browser DevTools Network tab
2. Look for requests to `/api/_temps/recordings`
3. Interact with your app
4. Check Temps dashboard for session replays
5. Verify sensitive data is masked/blocked
