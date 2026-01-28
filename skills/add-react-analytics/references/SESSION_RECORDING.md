# Session Recording

Privacy-aware session recording with visual replay functionality.

## Setup

Wrap your app with `SessionRecordingProvider`:

```tsx
// app/layout.tsx or app/providers.tsx
'use client';

import { SessionRecordingProvider } from '@temps-sdk/react-analytics';

export function Providers({ children }) {
  return (
    <SessionRecordingProvider
      enabled={true}
      maskAllInputs={true}       // Mask password/sensitive inputs
      maskAllText={false}         // Don't mask all text content
      blockClass="sensitive"      // CSS class to block from recording
      ignoreClass="ignore-recording"
    >
      {children}
    </SessionRecordingProvider>
  );
}
```

## Provider Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `enabled` | `boolean` | `true` | Enable/disable recording |
| `maskAllInputs` | `boolean` | `true` | Mask all input fields |
| `maskAllText` | `boolean` | `false` | Mask all text content |
| `blockClass` | `string` | - | CSS class to block elements |
| `ignoreClass` | `string` | - | CSS class to ignore elements |
| `sampling` | `object` | - | Sampling configuration |

## Control Recording

Use the `useSessionRecordingControl` hook to control recording:

```tsx
'use client';

import { useSessionRecordingControl } from '@temps-sdk/react-analytics';

function RecordingControls() {
  const { isRecording, startRecording, stopRecording, toggleRecording } =
    useSessionRecordingControl();

  return (
    <div>
      <p>Recording: {isRecording ? 'ON' : 'OFF'}</p>
      <button onClick={startRecording}>Start</button>
      <button onClick={stopRecording}>Stop</button>
      <button onClick={toggleRecording}>Toggle</button>
    </div>
  );
}
```

## Privacy Controls

### Block Sensitive Elements

Use CSS class or data attribute to block elements from recording:

```tsx
function PaymentForm() {
  return (
    <form>
      {/* Method 1: CSS class */}
      <input
        type="text"
        name="cardNumber"
        className="sensitive"
      />

      {/* Method 2: Data attribute */}
      <input
        type="text"
        name="cvv"
        data-rr-block
      />

      <button type="submit">Pay</button>
    </form>
  );
}
```

### Mask Text Content

For additional privacy, mask specific text:

```tsx
<div data-rr-mask>
  Sensitive text that will be masked in replay
</div>
```

### Common Privacy Patterns

```tsx
// Payment forms - block entirely
<div className="sensitive">
  <CreditCardForm />
</div>

// Personal info - mask inputs
<input type="text" name="ssn" data-rr-block />

// Medical info - block section
<section data-rr-block>
  <MedicalHistory />
</section>

// Financial data - mask display
<span data-rr-mask>${accountBalance}</span>
```

## GDPR Compliance

For GDPR-compliant recording:

```tsx
function ConsentBanner() {
  const [hasConsent, setHasConsent] = useState(false);
  const { startRecording, stopRecording } = useSessionRecordingControl();

  const handleAccept = () => {
    setHasConsent(true);
    startRecording();
    localStorage.setItem('recording_consent', 'true');
  };

  const handleDecline = () => {
    setHasConsent(false);
    stopRecording();
    localStorage.setItem('recording_consent', 'false');
  };

  return (
    <div>
      <p>We use session recording to improve our service.</p>
      <button onClick={handleAccept}>Accept</button>
      <button onClick={handleDecline}>Decline</button>
    </div>
  );
}

// In provider, check consent
<SessionRecordingProvider
  enabled={localStorage.getItem('recording_consent') === 'true'}
  maskAllInputs={true}
>
  {children}
</SessionRecordingProvider>
```

## Performance Considerations

Session recording adds ~2-3% CPU overhead. To minimize impact:

```tsx
<SessionRecordingProvider
  enabled={true}
  sampling={{
    mousemove: true,
    mouseInteraction: true,
    scroll: true,
    input: 'last',  // Only record final input value
  }}
>
  {children}
</SessionRecordingProvider>
```

## Troubleshooting

**Recording not working?**
- Verify `SessionRecordingProvider` wraps your app
- Check browser console for rrweb errors
- Ensure `enabled={true}`

**Sensitive data visible in replay?**
- Add `sensitive` class to containers
- Use `data-rr-block` on specific elements
- Enable `maskAllInputs={true}`

**Performance issues?**
- Reduce sampling rate
- Block large dynamic elements
- Disable on low-end devices
