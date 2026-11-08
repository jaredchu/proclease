# Release procedure

The GitHub [release workflow](../.github/workflows/release.yml) builds native
macOS ARM64/Intel and Linux ARM64/x64 archives. Each target runs formatting,
Clippy, the full lifecycle suite, an optimized build, an extracted-archive expiry
smoke test and installer checksum-failure checks. The smoke test confirms the
packaged command started before accepting its expiry, then waits for that job's
own supervisors before deleting its state; it retains the directory and reports
any that remain. No cross-compiled binary is
represented as runtime-tested.
Tag pushes build and draft a release. Relevant pull requests and manual workflow
runs validate the archives without publishing; ordinary main pushes use the
normal Rust and website checks. Every main push also deploys the current website
automatically; see [website publication](../website/README.md#publish).
Website-only updates that keep the existing release pins need no binary or crate
release.

For a new release:

1. Update Cargo versions, the pinned installer version, website download links,
   installer mirror and a `docs/releases/vVERSION.md` note. Validate the CLI,
   shared skill and website; preserve the normal contributor attribution.
   These pins name assets that exist only after publication, so keep the
   candidate on its own branch; merge it to main only when the release is public.
   Merging to main deploys the website, and its pin check fails before deployment
   if the release assets are not yet public.
2. Commit and push the reviewed candidate branch, then tag that exact commit `vVERSION`.
   Tag builds must all pass before the workflow assembles `SHA256SUMS` and creates
   a **draft prerelease** titled `ProcLease vVERSION experimental preview`, with
   four archives, their checksums and `install.sh`.
3. Download the draft assets, verify the combined checksums and inspect the notes.
   Check build logs for failed stripping tools and inspect the shipped binaries;
   a successful Cargo build alone does not prove stripping succeeded.
   Test the host-native archive/installer in isolated state. Publish the draft
   only after those checks pass; keep the experimental/prerelease flag.
4. Merge the candidate to main. The website workflow deploys it automatically,
   then verifies the live files; also confirm anonymous HTTPS access to the
   page, installer and release downloads. If deployment fails, fix the cause
   (through main if it is in source) and run the website workflow manually on
   current main.

Release archives include `proclease`, the portable skill, README and MIT license.
The packaging checks use Python 3.12 or newer, supplied in CI; users do not need
Python or Rust for binary installation. macOS Developer ID signing/notarization
and Homebrew distribution are not configured. Before promoting a stable macOS
release, add Developer ID signing and notarization and test a browser-downloaded,
quarantined artifact on a fresh Mac. These remain deferred for experimental
prereleases; basic ad hoc signatures are distinct from publisher identity.

## crates.io

The owner authorized crates.io publication of the CLI source, so the manifest
sets `publish = ["crates-io"]`. Its `include` list limits the source package to
`src/`, `tests/` (the lifecycle suite and its fixture), the portable skill,
README and license; Cargo adds `Cargo.toml` and `Cargo.lock`. The website,
workflows, helper scripts, agent instructions and other docs stay out of it.

Because the package omits `docs/` and other repository files, the README uses
portable links rather than relying on any registry rewriting of relative paths.
Links to files outside the package are absolute GitHub URLs: release-specific
files (release notes, skill, license) use `blob/vVERSION`, living contributor
documents use `blob/main`, and the terminal image uses its
`raw.githubusercontent.com/jaredchu/proclease/vVERSION/...` URL. Only in-page
`#anchor` links stay relative. Each target must exist at the named ref.

The manifest's `rust-version = "1.89"` matches the existing requirement of the
locked dependencies: `uuid 1.29.0` in `Cargo.lock` declares `rust-version =
"1.89.0"`. Source and registry builds therefore need Rust 1.89 or newer; the
prebuilt archives do not need Rust. Raising the requirement, or changing the
lockfile to lower it, is a separate reviewed change.

The release profile strips the shipped binary and uses thin LTO. Its
`build-override` leaves host build units, including procedural macros, unstripped:
on a macOS host, Rust 1.89 could not load a stripped `clap_derive` (E0463).
The `msrv-install` job in the [Rust checks](../.github/workflows/ci.yml) packages
the source and installs it with Rust 1.89 default release settings on hosted macOS
and Linux runners. Keep that job passing; do not replace it with a
`CARGO_PROFILE_*` environment override.

Publish only a version whose runtime source matches its GitHub release tag:

1. Confirm `src/`, `tests/`, `skills/` and `Cargo.lock` are byte-identical to
   `vVERSION`, the working tree is clean and the publication commit is reviewed.
   The crate's manifest metadata and README may differ from the binary tag; that
   publication commit is their provenance record.
2. Inspect `cargo package --list --locked`, then run `cargo package --locked`,
   which builds the extracted package. Running `cargo test --locked` inside the
   extracted package, with a short `PROCLEASE_TEST_ROOT`, checks that the suite
   is self-contained.
3. Run `cargo publish --dry-run --locked`. It needs crates.io index access but no
   token. A dry run with `--allow-dirty` is only preliminary evidence.
4. With an authorized crates.io login, run `cargo publish --locked` from the
   same clean commit. A crates.io version cannot be replaced; yanking only
   blocks new lockfile resolution.
5. After a successful upload, tag that publication commit `crates-vVERSION`.
   Never move the existing `vVERSION` binary-release tag.

[Docs.rs builds](https://docs.rs/about/builds) follow crates.io publication; its
[builder command](https://github.com/rust-lang/docs.rs/blob/main/crates/lib/docs_rs_rustwide/src/args.rs)
runs `cargo rustdoc --lib`.
ProcLease has only a binary target, so docs.rs shows a crate overview,
README and source rather than an API. No docs.rs metadata is configured.

Current registry release: [proclease 0.1.6](https://crates.io/crates/proclease/0.1.6),
with a [docs.rs crate overview](https://docs.rs/crate/proclease/0.1.6).
The [v0.1.6 publication](verification.md#v016-publication-2026-11-08) records
fresh binary and default Rust/Cargo 1.89 registry installs, including readiness,
keyed retries with unchanged deadlines and expiry checks without profile overrides.
The earlier [v0.1.5 publication](verification.md#v015-publication-2026-11-08)
records bulk-stop installation checks.
Current stable Rust/Cargo remains recommended. The immutable published 0.1.3
crate predates this fix; its earlier
[caveat and workaround](verification.md#registry-and-website-controls-2026-11-07)
remain documented for users of that version.
