# ProcLease website

The public website uses monochrome technical minimalism with a dark theme.
`dist/` contains buildless HTML, CSS, JavaScript and local assets; no package
installation or frontend framework is required. The terminal interaction is an
explicitly labelled illustration and starts no real processes.

The production target is a static-assets-only Cloudflare Worker named
`proclease-site`, configured in `wrangler.toml`. The custom domain is
`proclease.com`; Cloudflare manages its domain binding and TLS. There is no
server script, database, tracking or account system.

## Check and preview

Keep the installer mirror current after editing the root installer:

```sh
cp install.sh website/dist/install.sh
python3 scripts/check-website.py
node --check website/dist/app.js
sh -n install.sh
proclease run --owner codex --task website-preview --ttl 30m -- python3 -m http.server 4387 --bind 127.0.0.1 --directory website/dist
```

Visit `http://127.0.0.1:4387`, then stop the returned job when finished. Browser
checks cover the lease demo, replay, clipboard, keyboard tabs, skill and platform
choices, FAQ and horizontal bounds at 320/390/768/1440 pixels, plus 200% text
at 320 and 390 pixels.

## Publish

Use an authorized Cloudflare login and explicitly select the account that owns
the domain through `CLOUDFLARE_ACCOUNT_ID`; keep credentials out of this repository.
After checks and review, deploy the committed static files:

```sh
wrangler deploy --config website/wrangler.toml
```

Check the live page, installer bytes and links to published release assets.
Use curl or an identifying application User-Agent for automated HTTP checks;
Cloudflare's Browser Integrity Check rejects the default Python `urllib` signature.
See [Cloudflare's explanation](https://developers.cloudflare.com/support/troubleshooting/http-status-codes/cloudflare-1xxx-errors/error-1010/).
Release links and the installer are pinned to the published v0.1.4 prerelease.
Update those pins together for a future release, and publish its matching
release assets before deploying the website.
See [the release procedure](../docs/releasing.md).

## Identity and type

The mark in `dist/assets/icon.svg` is a lease ring: an open arc for the
remaining lease, a tick for the deadline and a solid block for the supervised
process. Header, footer, SVG favicon, `favicon.ico` (16/32/48),
`assets/icon-32.png`, `assets/apple-touch-icon.png` (180) and the 1200x630
`assets/social.png` all use the same geometry. The PNG and ICO files are rendered
from that SVG; edit the SVG first and re-render. Type uses a rem-based fluid
scale (12px minimum labels, 16px body), so browser text size settings apply.
Website copy and demo states avoid em dashes; unavailable values read `n/a`.
The skill and platform choices are native radio groups rather than selects, so
long labels wrap at narrow widths and enlarged text.

## Interface icons

Interface icons come from [Tabler Icons v3.50.0](https://github.com/tabler/tabler-icons/releases/tag/v3.50.0)
(MIT, Copyright (c) 2020-2026 Paweł Kuna). Path data is copied unchanged from the
tag's `icons/outline` and `icons/filled` static SVGs
([format](https://docs.tabler.io/icons/static-files/svg)). Only the icons in use
are bundled, as `<symbol>` elements in a hidden sprite at the top of
`dist/index.html`; each records its upstream file in `data-source`. There is no
icon font, CDN or runtime dependency. The upstream license is served unchanged
at `dist/assets/tabler-icons/LICENSE.txt`.

| Use | Tabler icon |
| --- | --- |
| External links (header, release tags, source, installer, skill, footer) | `outline/arrow-up-right` |
| Install, Get ProcLease and `.tar.gz` download | `outline/download` |
| Demo terminal title | `outline/terminal` |
| Release tag and demo status: running, stopping / expired | `filled/point` / `outline/point` |
| Demo URL and caption note | `outline/corner-down-right` |
| Demo advance / replay | `outline/arrow-right` / `outline/rotate` |
| FAQ disclosure (rotates to a cross when open) | `outline/plus` |
| Copy button; copy succeeded / unavailable | `outline/copy`; `outline/check` / `outline/alert-circle` |

Icons are 1em and inherit `currentColor`. Outline icons use stroke width 2; the
filled status point retains its upstream fill.
They are decorative (`aria-hidden="true"`, `focusable="false"`) next to visible
text, so accessible names come from that text or an existing `aria-label`. Script
swaps only a `<use>` reference and its label text for demo and copy states.
Textual punctuation, separators and command syntax (`$`, `/`, `+`, `·`) stay as
text. `scripts/check-website.py` checks that every symbol is used and every
reference resolves, that the license notice is present, and that the replaced
glyphs (`↗ ↓ → ↺ ↳ ● ○ >_`) are absent; geometry against upstream is checked at review.

## Search and answer-engine decisions

The refresh applies ordinary, verifiable practices rather than special AI tricks:

| Decision | Source |
| --- | --- |
| One canonical URL shared by `<link rel="canonical">`, `og:url` and the sitemap | [NavSEO: canonical tags](https://navseo.com/learn/technical-seo/canonical-tags-and-duplicate-content/) |
| Definition, FAQ, links and metadata in the initial HTML; JavaScript only drives the demo and installer controls | [NavSEO: JavaScript SEO](https://navseo.com/learn/technical-seo/javascript-seo-and-rendering/) |
| Descriptive navigation, including FAQ and Docs, visible on mobile and desktop; keyboard focus kept | [NavSEO: navigation SEO](https://navseo.com/learn/on-page-seo/navigation-seo/) |
| An early plain definition, concise FAQ answers with stated limits, and source/docs/support/license links | [NavSEO: AI citation SEO](https://navseo.com/learn/ai-citation-seo/what-ai-citation-seo-means/) |
| Small JSON-LD `WebSite` plus `SoftwareApplication` matching visible facts; no ratings, offers, organization or version | [NavSEO: structured data](https://navseo.com/learn/ai-citation-seo/structured-data-and-ai-visibility/), [schema.org SoftwareApplication](https://schema.org/SoftwareApplication) |
| Specific title and description naming the product and task | Product facts in the root README |

Primary platform reference: [Google Search AI features](https://developers.google.com/search/docs/appearance/ai-features)
states that standard SEO practices apply and that no special schema or AI text
file is required. The site therefore has no `llms.txt`, and robots, crawler
policy and hosting are unchanged. These measures do not guarantee rankings, rich
results or AI citations. `scripts/check-website.py` validates local structure only
(JSON parsing, canonical consistency, icons and dimensions, references, em dash
absence); it does not run search-engine validators.
