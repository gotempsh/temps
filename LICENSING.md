# Licensing

Temps is licensed under the **Functional Source License, Version 1.1, Apache 2.0 Future License** (FSL-1.1-Apache-2.0). The full legal text is in [LICENSE.md](LICENSE.md). This page explains what that means in plain English.

## TL;DR

- **Self-host Temps for free, forever.** Personal, internal company use, education, research — all permitted, including for commercial businesses.
- **You can fork it, modify it, and redistribute it** as long as you keep the license intact.
- **You cannot use Temps to launch a competing hosted PaaS** that substitutes for Temps Cloud.
- **After 2 years, every release becomes Apache 2.0.** Truly open source on a 2-year delay.

## What you *can* do

| Use case | Allowed? |
|---|---|
| Self-host Temps for your own apps | Yes |
| Self-host Temps for your company's internal use | Yes |
| Self-host Temps for your client projects (you build apps on it) | Yes |
| Use Temps for non-commercial education or research | Yes |
| Fork Temps and modify it for your own use | Yes |
| Contribute changes back via PRs | Yes — encouraged |
| Offer paid consulting, support, or implementation services around Temps | Yes |
| Build and sell a SaaS product *on top of* Temps (e.g. your app deployed on Temps) | Yes |
| Use a release of Temps that is 2+ years old under Apache 2.0 | Yes |

## What you *cannot* do

You cannot make Temps available to others as a commercial product or service that competes with Temps Cloud or with Temps itself. Specifically, you may not:

- Resell Temps as a managed/hosted "deploy-your-app" platform to third parties.
- Launch "Acme Cloud, powered by Temps" as a paid PaaS for other companies.
- Strip the Temps branding and offer the same functionality as a paid product.

If you want to do any of the above, contact us at **dviejokfs@temps.sh** for a commercial license.

## Why FSL and not MIT/Apache or AGPL?

We tried to balance three things:

1. **Users own their infrastructure.** Self-hosting must be free, frictionless, and free of copyleft virality (so you don't have to worry about your own app's source code being affected — unlike AGPL).
2. **The project survives.** Temps Cloud funds Temps development. If a hyperscaler takes our code and undercuts us, that funding disappears and so does the project. FSL prevents that without restricting normal users.
3. **Long-term openness.** FSL converts to Apache 2.0 after 2 years. So even if Temps the company stops existing, every release older than 2 years is permanently open source. This is a stronger guarantee than proprietary or even BUSL.

## Comparison with similar projects

| Project | License | Self-host free | Competing service blocked | Becomes OSS |
|---|---|---|---|---|
| **Temps** | FSL-1.1-Apache-2.0 | Yes | Yes | After 2 years (Apache 2.0) |
| Sentry | FSL-1.1-Apache-2.0 | Yes | Yes | After 2 years (Apache 2.0) |
| HashiCorp Terraform | BUSL-1.1 | Yes | Yes | After 4 years (MPL 2.0) |
| Elastic | Elastic License v2 | Yes | Yes | Never |
| Grafana | AGPL-3.0 | Yes (with copyleft) | No | Already OSS |
| Plausible | AGPL-3.0 | Yes (with copyleft) | No | Already OSS |
| Vercel / Railway | Proprietary | No | N/A | Never |

## Contributing

By contributing to Temps you agree that your contributions are licensed under the same FSL-1.1-Apache-2.0 license. We do not require a CLA. Each release of your contribution will become Apache 2.0 two years after that release is published, identically to the rest of the codebase.

## Trademarks

"Temps" and the Temps logo are trademarks of the Temps project. The license lets you use the *code*; it does not grant rights to use the name or logo for your own product. You can absolutely say "powered by Temps" or "uses Temps" — we just ask that you don't pretend your fork *is* Temps.

## Questions

- Commercial licensing: **dviejokfs@temps.sh**
- General license questions: open an issue on [GitHub](https://github.com/gotempsh/temps/issues)
- Security: see [SECURITY.md](SECURITY.md)

## Disclaimer

This page is a plain-English summary, not legal advice. The legally binding terms are in [LICENSE.md](LICENSE.md). If there is any conflict between this page and the LICENSE, the LICENSE controls.
