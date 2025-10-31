## Adding Analytics to Remix

Follow these steps to integrate Temps analytics into your Remix application:

### Step 1: Install the SDK

```bash
npm install @temps-sdk/react-analytics
# or
yarn add @temps-sdk/react-analytics
# or
pnpm add @temps-sdk/react-analytics
# or
bun add @temps-sdk/react-analytics
```

### Step 2: Add the Analytics Provider

Wrap your app with the provider in `app/root.tsx`:

```typescript
// app/root.tsx
import { TempsAnalyticsProvider } from '@temps-sdk/react-analytics';
import {
  Links,
  Meta,
  Outlet,
  Scripts,
  ScrollRestoration,
} from '@remix-run/react';

export default function App() {
  return (
    <html lang="en">
      <head>
        <meta charSet="utf-8" />
        <meta name="viewport" content="width=device-width, initial-scale=1" />
        <Meta />
        <Links />
      </head>
      <body>
        <TempsAnalyticsProvider basePath="/api/_temps">
          <Outlet />
        </TempsAnalyticsProvider>
        <ScrollRestoration />
        <Scripts />
      </body>
    </html>
  );
}
```

### Step 3: Track Custom Events

Use the `useTrackEvent` hook in any route component:

```typescript
import { useTrackEvent } from '@temps-sdk/react-analytics';

export default function MyRoute() {
  const trackEvent = useTrackEvent();

  const handleClick = () => {
    trackEvent('button_click', {
      button_id: 'subscribe',
      page: '/pricing',
      plan: 'premium'
    });
  };

  return (
    <button onClick={handleClick}>
      Subscribe Now
    </button>
  );
}
```

### Step 4: Identify Users (Optional)

Associate analytics with specific users:

```typescript
import { useAnalytics } from '@temps-sdk/react-analytics';
import { useEffect } from 'react';

export default function UserProfile({ user }) {
  const { identify } = useAnalytics();

  useEffect(() => {
    if (user) {
      identify(user.id, {
        email: user.email,
        name: user.name,
        plan: user.subscription?.plan
      });
    }
  }, [user, identify]);

  return <div>Profile content</div>;
}
```

### Step 5: Track Scroll Depth (Optional - Great for Landing Pages & Blogs)

Track how far users scroll on your pages:

```typescript
import { useScrollAnalytics } from '@temps-sdk/react-analytics';

export default function BlogPost() {
  useScrollAnalytics({
    thresholds: [25, 50, 75, 100], // Track at 25%, 50%, 75%, and 100% scroll
    onScroll: (percentage) => {
      console.log(`User scrolled to ${percentage}%`);
    },
  });

  return (
    <article>
      <h1>Your Blog Post Title</h1>
      <p>Long content here...</p>
    </article>
  );
}
```

**Perfect for:**
- Landing pages (track engagement)
- Blog posts (measure readership)
- Product pages (understand user interest)
- Documentation (see how far users read)

### Step 6: Verify Installation

1. **Deploy your changes** - Push to staging or production
2. **Visit your application** - Navigate through a few pages
3. **Check the Analytics Dashboard** - View real-time data in Temps
4. **Debug if needed** - Check browser console for any errors
