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
normal Rust and website checks.

For a new release:

1. Update Cargo versions, the pinned installer version, website download links,
   installer mirror and a `docs/releases/vVERSION.md` note. Validate the CLI,
   shared skill and website; preserve the normal contributor attribution.
   These pins name assets that exist only after publication, so keep the
   candidate on its own branch; merge it to main only when the release is public.
2. Commit and push the reviewed source, then tag that exact commit `vVERSION`.
   Tag builds must all pass before the workflow assembles `SHA256SUMS` and creates
   a **draft prerelease** titled `ProcLease vVERSION experimental preview`, with
   four archives, their checksums and `install.sh`.
3. Download the draft assets, verify the combined checksums and inspect the notes.
   Test the host-native archive/installer in isolated state. Publish the draft
   only after those checks pass; keep the experimental/prerelease flag.
4. Publish the matching website from its committed source and verify anonymous
   HTTPS access to the page, installer and release downloads.

Release archives include `proclease`, the portable skill, README and MIT license.
The packaging checks use Python 3.12 or newer, supplied in CI; users do not need
Python or Rust for binary installation. macOS Developer ID signing/notarization,
package registries and Homebrew distribution are not configured.

The manifest keeps `publish = false`. `cargo package --list` and a local
`cargo package` build show what a crates.io source package would contain.
[Docs.rs builds](https://docs.rs/about/builds) follow crates.io publication; its
[builder command](https://github.com/rust-lang/docs.rs/blob/main/crates/lib/docs_rs_rustwide/src/args.rs)
runs `cargo rustdoc --lib`.
ProcLease has only a binary target, so docs.rs would show a crate overview,
README and source rather than an API. Registry publication is a separate decision.
