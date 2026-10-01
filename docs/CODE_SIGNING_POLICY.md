# Code signing policy

Free code signing provided by [SignPath.io](https://about.signpath.io/),
certificate by [SignPath Foundation](https://signpath.org/).

## What is signed

Only binaries built from this repository's source, by the
[release workflow](../.github/workflows/release.yml) on GitHub-hosted runners,
from a `v*` tag. Each release signs:

| File | What it is |
|---|---|
| `relay-ui.exe` | the app window (Tauri shell) |
| `relay-core.exe` | the always-on background service |
| `relay-svc.exe` | windowless launcher for the service |
| `relay-elevate.exe` | the elevated install helper (the only Relay binary that writes HKLM) |
| `relay-share.exe` | the screen/audio share engine, started per share |
| `relay-preview.exe` | the offline A/B audio renderer |
| `relay_apo.dll` | the audio effect (only registered if the user opts in) |
| `relay_vdevice.dll` | the virtual camera media source (only registered if the user opts in) |
| `Relay_<version>_x64-setup.exe` | the NSIS installer containing all of the above |

The exact list is enforced by [`.signpath/artifact-configuration.xml`](../.signpath/artifact-configuration.xml);
a build that produces anything else is rejected. Third-party code is not
signed with this certificate.

## Who can sign

| Role | Who |
|---|---|
| Committers and reviewers | [@jakesterns](https://github.com/jakesterns) |
| Approvers | [@jakesterns](https://github.com/jakesterns) |

Every change to `main` needs the owner's review (see `CODEOWNERS`), external
contributions included. Every signing request is approved manually by the
owner in SignPath, once per release; nothing is signed automatically. All team
members use multi-factor authentication on GitHub and SignPath.

## Privacy

Relay collects no telemetry, analytics or crash reports, and has no accounts.
Screen and audio sharing goes directly between your own two PCs on your local
network (WebRTC, encrypted with DTLS-SRTP); no Relay server is involved.

The only other outbound request Relay makes is user-initiated: when you pick a
headphone model and ask for its measured curve, Relay fetches that one file
over HTTPS from the AutoEQ project's GitHub repository
(`raw.githubusercontent.com`; see
[GitHub's privacy statement](https://docs.github.com/site-policy/privacy-policies/github-general-privacy-statement)).
You can skip it by pasting a curve instead. Nothing about you or your PC is sent.

## System changes

Relay changes nothing system-wide without asking. The audio effect and the
virtual camera are separate opt-ins at first run, each with a description of
what is installed and how to remove it. The uninstaller removes every
registration Relay made and restores the previous state.
