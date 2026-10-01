# SignPath Foundation application (ready to paste)

The form is at <https://signpath.org/apply>. Terms: <https://signpath.org/terms>.
Answers below; copy each into the matching field. Contact details (name,
email) are typed into the form by the owner and are deliberately not in this
public file.

## Form answers

**Project name**
Relay

**Repository URL**
https://github.com/jakesterns/relay

**Project homepage / download page**
https://github.com/jakesterns/relay (releases: https://github.com/jakesterns/relay/releases)

**Licence**
MIT (OSI-approved). No commercial dual licensing, no proprietary components.
Bundled third-party assets and their licences are listed in
`THIRD_PARTY_NOTICES.md` and in the `licenses.html` shipped with every release.

**Short description**
Relay is a free, open-source Windows desktop app that shares one PC's screen
and audio to a second PC on the same local network, where it appears as a
camera and microphone for Discord, Zoom or Meet: a software replacement for a
capture card. It also applies per-game display colour and audio profiles that
switch on only while the chosen game has focus and are restored afterwards.

**Maintainers / team**
@jakesterns (GitHub) - author, reviewer and release approver.

**Code signing policy URL**
https://github.com/jakesterns/relay/blob/main/docs/CODE_SIGNING_POLICY.md

**Build system**
GitHub Actions, GitHub-hosted `windows-latest` runners, workflow
`.github/workflows/release.yml`, triggered only by pushing a `v*` tag in the
upstream repository. Submission uses `signpath/github-action-submit-signing-request`
so SignPath's origin verification sees the repository, commit and workflow.

**What will be signed** (Authenticode, x64)
- `relay-ui.exe` - app window (Tauri 2)
- `relay-core.exe` - background service
- `relay-svc.exe` - windowless launcher for the service
- `relay-elevate.exe` - elevated install helper
- `relay-share.exe` - share engine
- `relay-preview.exe` - offline audio renderer
- `relay_apo.dll` - user-mode audio processing object (opt-in)
- `relay_vdevice.dll` - virtual camera media source (opt-in)
- `Relay_<version>_x64-setup.exe` - NSIS installer

All are built from this repository's Rust and TypeScript sources. No
third-party binaries are signed. No kernel-mode drivers are included or
requested.

**Why signing matters for users**
Relay ships an installer and an elevated helper that registers an optional
audio effect and a virtual camera. Unsigned, every download triggers
SmartScreen's "unknown publisher" block and the UAC prompt shows "Unknown
publisher" for the helper, which teaches users to click through warnings for
exactly the component that most deserves scrutiny. A signature lets users and
antivirus products tie each binary to this repository's public build, and lets
the Windows virtual camera frame server and audio engine identify the
components they load.

**Reproducibility / verifiability**
- Release builds run only in GitHub Actions from a tag; nothing is built on a
  developer machine.
- `scripts/repro-flags.ps1` strips build paths and timestamps from the Rust
  binaries (see `docs/dev/reproducible-builds.md`), so the same commit and
  toolchain produce the same bytes.
- Lockfiles are committed (`Cargo.lock`, `ui/pnpm-lock.yaml`) and installed
  with `--locked` / `--frozen-lockfile`.
- `cargo-deny` and `cargo-about` gate dependency licences in CI.
- Each release publishes SHA-256 sums of the signed artifacts.

**Privacy**
No telemetry, analytics, crash reporting or accounts. Sharing is
peer-to-peer on the LAN. The only other outbound request is a user-initiated
HTTPS fetch of one headphone measurement file from the AutoEQ GitHub
repository, skippable by pasting a curve.

**Release status**
Pre-release; first signed release will be the first tagged public release.
(SignPath's terms ask for a project that is "already released": if the
application is declined on that ground, publish an unsigned `v0.x` release
first and reapply.)

## Owner checklist

1. **Before applying:** add Windows version resources (ProductName "Relay",
   ProductVersion) to `relay-share.exe`, `relay-preview.exe`, `relay_apo.dll`
   and `relay_vdevice.dll` - today only relay-core's three binaries and
   relay-ui carry them, and SignPath Foundation enforces product name and
   version on every signed file. Enable MFA on GitHub if not already on.
2. **Apply:** paste the answers above into <https://signpath.org/apply> and submit.
3. **When approved:** in SignPath, enable MFA; create the project (slug e.g.
   `relay`), link the GitHub repository as a trusted build system, add two
   artifact configurations with slugs `binaries` and `installer` from
   `.signpath/artifact-configuration.xml`, note the signing policy slug, and
   create a submitter API token.
4. **In GitHub:** Settings > Environments > New environment `release`, add
   yourself as required reviewer, restrict it to tags `v*`, then add secret
   `SIGNPATH_API_TOKEN` and variables `SIGNPATH_ORGANIZATION_ID`,
   `SIGNPATH_PROJECT_SLUG`, `SIGNPATH_SIGNING_POLICY_SLUG`.
5. **Release:** `git tag v0.1.0 && git push origin v0.1.0`, approve the run in
   GitHub and the two signing requests in SignPath, then review and publish
   the draft GitHub Release the workflow creates.

## What a SignPath signature does not cover

SignPath Foundation issues a standard (OV) Authenticode certificate. It does
not do Microsoft Hardware Dev Center submissions, so it cannot produce a
Microsoft-signed kernel driver. A kernel-mode virtual audio driver needs
Microsoft attestation or WHQL signing, which needs an EV certificate on a
Partner Center hardware account. Whether `relay_apo.dll` loads in protected
`audiodg.exe` with only an OV signature is covered in the PR description and
must be confirmed on a stock machine before the per-app EQ plan relies on it.
