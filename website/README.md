# ProcLease website

The public website uses monochrome technical minimalism with a dark theme.
`dist/` contains buildless HTML, CSS, JavaScript and local assets; no package
installation or frontend framework is required. The terminal interaction is an
explicitly labelled illustration and starts no real processes.

The production target is a static-assets-only Cloudflare Worker named
`proclease-site`, configured in `wrangler.toml`. The custom domain is
`proclease.com`; Cloudflare manages its domain binding and TLS. There is no
server script, database, tracking or account system.

## Canonical URL and redirects

The canonical address is `https://proclease.com`. A Cloudflare zone Single
Redirect sends HTTP apex requests and both HTTP/HTTPS `www` requests there with
status 301, preserving the path and query string in one hop. HTTPS apex requests
continue to the static Worker.

The zone's `http_request_dynamic_redirect` entry point contains an enabled rule
with this filter:

```text
(http.host eq "www.proclease.com") or (http.host eq "proclease.com" and not ssl)
```

Its dynamic target is `concat("https://proclease.com", http.request.uri.path)`,
with `preserve_query_string: true`. The redirect-only `www` DNS record is a
proxied AAAA record with content `100::` and automatic TTL, following
[Cloudflare's redirect-domain guidance](https://developers.cloudflare.com/fundamentals/manage-domains/redirect-domain/).
Universal SSL supplies the public certificate. The active zone uses full DNS
setup; `www` does not need a Worker custom-domain binding.

These DNS and zone rules are managed separately from `wrangler.toml`; the website
workflow deploys assets and does not recreate them. After changing this setup,
check HTTP apex, HTTP/HTTPS `www`, and HTTPS apex with an identifying curl
User-Agent. Include a nested path and query string, confirm exactly one redirect
to the canonical address, and verify HTTPS without disabling certificate checks.
Public DNS caches can briefly retain an earlier missing-record response.

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

[`website.yml`](../.github/workflows/website.yml) deploys automatically. Pull
requests that touch the website, installer, website checker or workflow run the
static checks; feature branches are validated through those pull requests. Every
push to `main` in `jaredchu/proclease` runs the checks and, when they pass,
deploys `dist/` with `wrangler deploy --config website/wrangler.toml`, even when
the push changes no website files. Merging reviewed changes to main therefore
publishes the current site to proclease.com. That covers this static Worker
only. GitHub releases, crates, DNS, new domains and global skill installation
keep their own scope; see [the release procedure](../docs/releasing.md).

The deploy job never runs for pull requests (including same-repository ones),
other branches, tags or forks, and does not run when checks fail. It first
requires the exact canonical repository and `refs/heads/main`, because workflow
expression comparisons ignore case. Before deploying, it checks that every
release pin in `install.sh`, `index.html` and `app.js` names the installer's
version, and that its published `SHA256SUMS` lists four downloadable archives.

Main runs share one concurrency group: one runs at a time, an in-flight run is
not cancelled, and a newer pending run replaces an older pending one. GitHub
does not guarantee that this queue follows push order, and a re-run keeps its
original commit. After the pin check and before the Wrangler action (which installs the CLI),
the job reads the current `refs/heads/main` from GitHub. If the run's commit is no longer
the head of main, it skips deployment and live verification with a notice; the
run for the newer push deploys it. If no newer run is queued, for example after
GitHub replaced it while pending, run the workflow manually on `main`. If the
lookup fails or returns an unexpected result, the job fails without deploying. A push that lands after this check is
deployed by its own run, which waits for the current one.

A stale skip can still create a green GitHub environment record. To confirm
publication, check that both "Deploy static Worker" and "Verify live files match
this commit" ran successfully; the environment badge alone is insufficient.

After deploying, the job fetches every tracked `dist/` file from proclease.com,
including `/` and `/install.sh`, and requires bytes identical to the commit.
Each pass requests every file in turn, allowing 20 seconds per request, and
waits 10 seconds before retrying failed passes, up to 12 passes. The step is
hard-limited to 3 minutes and fails when that limit or the last pass is reached;
the deploy job's 15-minute limit, shared with setup, the pin check and the
deployment, can end it earlier.

The Wrangler action is pinned to a full commit SHA and Wrangler to an exact
version; update them deliberately.

### Credentials

Deployment uses two secrets of the `production` GitHub environment, referenced
only by the deploy step:

- `CLOUDFLARE_API_TOKEN`: an account-owned Cloudflare API token limited to the
  one account that owns the `proclease-site` Worker, with Workers Scripts Write
  on that account and Workers Routes Write plus Zone Read on the `proclease.com`
  zone. Grant no other accounts, zones, DNS, billing or unrelated permissions.
- `CLOUDFLARE_ACCOUNT_ID`: that account's ID.

Create the `production` environment with a deployment-branch rule allowing only
`main`, and store both values there rather than as repository secrets. That rule
also keeps a workflow edited on another branch from reaching them. See Cloudflare's
[GitHub Actions guide](https://developers.cloudflare.com/workers/ci-cd/external-cicd/github-actions/).
Never commit credentials or print them in logs.

To rotate, create the replacement token, update the environment secret, run the
workflow manually on `main` and then revoke the old token. If a deployment fails
or must be repeated, fix the cause and run the workflow manually with `main`
selected; that run uses the current head of main. Re-running an older run
deploys only if its commit is still the head of main; otherwise it skips. A
manual run on another branch checks without deploying. A local Wrangler
deployment is not part of the normal process; it needs its own authorization,
should deploy only the current main commit and must be followed by the same live
checks.

Use curl or an identifying application User-Agent for automated HTTP checks;
Cloudflare's Browser Integrity Check rejects the default Python `urllib` signature.
See [Cloudflare's explanation](https://developers.cloudflare.com/support/troubleshooting/http-status-codes/cloudflare-1xxx-errors/error-1010/).
Release links and the installer are pinned to the v0.1.6 prerelease.
Update those pins together for a future release, and merge them to main only
after its matching release assets are public; the pin check blocks deployment
otherwise. See [the release procedure](../docs/releasing.md).

The hero intro and the "Why choose ProcLease?" FAQ link to the source-backed
[comparison](../docs/comparison.md) on main. The homepage describes explicit
expiry and scoped cleanup. Optional bounded readiness checks ship in v0.1.6;
the README and shared skill document their observation-only limits.

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
