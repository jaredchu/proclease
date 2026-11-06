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
checks cover the lease demo, replay, clipboard, keyboard tabs, platform download
selection, FAQ and horizontal bounds at 320/390/768/1440 pixels.

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
Release links and the installer are pinned to v0.1.2. Update those together for
a future release. See [the release procedure](../docs/releasing.md).
