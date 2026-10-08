# NDI® licensing for Relay (S51, bundled in RC2)

NDI® is a registered trademark of Vizrt NDI AB. More about NDI: https://ndi.video/

First read 2026-10-07 (S51); re-verified 2026-10-07 for RC2, when the owner
chose **option B: bundle the NDI runtime** so NDI output works with nothing
else to install. This is an engineering reading of the published terms, not
legal advice; bundling is still worth putting past a lawyer.

## Verdict

1. **Relay loads the NDI runtime dynamically.** `relay-share` calls
   `LoadLibraryExW` on the full path of `Processing.NDI.Lib.x64.dll` and
   `GetProcAddress` on each C function it uses. The FFI declarations are
   written by hand in `crates/capture/src/ndi/ffi.rs`. No NDI SDK header,
   library, import lib or binary is committed to the repository, and none is
   needed to build Relay.
2. **Release installers bundle the runtime (option B).** The installer puts
   NDI's own `Processing.NDI.Lib.x64.dll` (6.3.2.0) and NDI's notice file
   `Processing.NDI.Lib.Licenses.txt` in Relay's install folder
   (`%LOCALAPPDATA%\Relay`, next to `relay-share.exe`), never a system path
   (S2). Both are taken, byte for byte, out of NDI's public redistributable
   (S3, S7) at release-build time and checked against pinned SHA-256s and
   Vizrt's Authenticode signature. They never enter the repository (S1 §2d).
3. **Search order:** `RELAY_NDI_RUNTIME` (tests; replaces the rest), then
   Relay's own folder (the bundled copy), then `NDI_RUNTIME_DIR_V6`, then the
   NDI 6 runtime's default folder (`%ProgramFiles%\NDI\NDI 6 Runtime\v6`).
   `NdiRuntime.bundled` says which one was found; the NDI card shows
   "Included with Relay" or "Installed from NDI". Without either, the card
   still says "NDI output needs the NDI runtime" and links NDI's download.
4. **Every installer bundles it.** `stage-bundle.ps1` fetches and verifies
   the pinned runtime on every build, local, test or release, and fails the
   build if it is missing or does not match: a test installer without it
   did not test what ships (PC2, r56: a 4.7 MB installer with no NDI). The
   download is cached once per machine in
   `%LOCALAPPDATA%\RelayBuildCache\ndi-runtime\<version>` (or under
   `RELAY_BUILD_CACHE`) and re-verified before each use. `-NoNdi` is the
   explicit offline-development opt-out: `binaries\ndi` stays empty, it
   warns, and NDI output falls back to an installed runtime. Plain
   `cargo build` never needs it.
   An update over a running install moves a still-loaded copy aside instead
   of skipping it (`crates/core/src/update_files.rs`).
5. **Licence terms:** the installer's licence page
   (`ui/src-tauri/installer/license.txt`, Tauri `bundle.licenseFile`) is
   Relay's MIT licence (Part 1) plus the NDI terms S1 §3d requires (Part 2).
   Notices: trademark line and https://ndi.video/ link next to every NDI
   toggle and in the About card, `THIRD_PARTY_NOTICES.md` §4 (which
   `licenses.html` includes), and the README. No "NDI" in the product name.

## Sources

Re-read 2026-10-07. S1 is the PDF dated 2025-08-21 (unchanged since S51;
text extracted and checked against the quotes below). S2–S4 re-fetched; the
*Software Distribution* page still reads "You may distribute these files
within your application if your EULA terms cover the specific requirements
of the NDI SDK EULA" and, for the redistributable, "you must make all
reasonable efforts to keep the versions you distribute up to date".

| # | Source | What it says (quoted or closely paraphrased) |
|---|---|---|
| S1 | NDI SDK License Agreement, https://downloads.ndi.tv/SDK/NDI_SDK/NDI%20SDK%20License%20Agreement.pdf (PDF dated 2025-08-21; also linked as http://ndi.link/ndisdk_license) | §2a royalty-free licence "to distribute, only in accordance with the SDK Documentation requirements, object code included in the SDK solely as used by such Products". §2b "The Bundled Product must incorporate and be compatible with the latest version of the SDK available at the time of such use, development, or distribution". §2d "Unless otherwise stated in the SDK, no files within the SDK and the Specific SDK may be distributed. Certain files ... may be distributed, said files and their respective distribution license are individually identified within the SDK documentation." §3d any distribution of your Product must be "under the terms of a license agreement containing terms that: (i) prohibit any modifications to the SDK ... (ii) prohibit any reverse engineering, disassembly or recompilation ... (iii) prohibit any circumvention of any technical limitations ... (iv) [prohibit] removal ... of any proprietary notices", disclaim warranties and liability on NDI's behalf, comply with US export law, and "include the appropriate copyright notice showing NDI ... as copyright owner". §3f NDI trademarks only "to identify that the Product is compatible with NDI Products, and in all cases ... special and clear notations shall be provided that the marks are NDI trademarks"; no suggestion of sponsorship. §3h Products must stay interoperable with other NDI products. §1b "Products" are software for general-purpose computers the user can freely change (desktops qualify; locked appliances and embedded devices need a commercial licence). Governing law: Sweden. |
| S2 | NDI docs, *Licensing*, https://docs.ndi.video/all/developing-with-ndi/sdk/licensing | Provide "a link to ndi.video in a location close to all locations where NDI is used/selected within the product, on your website, and in its documentation." Use NDI only as "NDI®" with "NDI® is a registered trademark of Vizrt NDI AB" on the same page near the first use or in a footnote; the About box must carry the same statement. Do not distribute NDI Tools; link to https://ndi.video/tools/. "You should include the NDI DLLs as part of your own application and keep them in your application folders" — never in system paths. Contact NDI before using "NDI" in a product name. Codec (AAC/H.264/H.265) licensing is the developer's responsibility. |
| S3 | NDI docs, *Software Distribution*, https://docs.ndi.video/all/developing-with-ndi/sdk/software-distribution | Header files "may be distributed with open-source projects under the terms of the MIT license". Binaries are distributable within applications provided the application's EULA covers the NDI SDK requirements. The redistributable installers may be included in your installer (silently, `/verysilent`) provided the licence terms are covered in your documentation and you keep them current; or "provide a user link to the NDI-provided download". |
| S4 | NDI docs, *Dynamic Loading of NDI Libraries*, https://docs.ndi.video/all/developing-with-ndi/sdk/dynamic-loading-of-ndi-libraries | Dynamic loading is a supported way to use NDI. The runtime may sit in the application folder, or "on Windows, you can install the NDI runtime and use an environment variable to locate it on disk"; `NDILIB_REDIST_URL` is the download location, e.g. http://ndi.link/NDIRedistV6. |
| S5 | NDI SDK header `Processing.NDI.Lib.h` (as published in open-source projects, e.g. https://github.com/DistroAV/DistroAV/blob/master/lib/ndi/Processing.NDI.Lib.h) | MIT licence text in the header; defines `NDILIB_REDIST_FOLDER` as the `NDI_RUNTIME_DIR_V6` environment variable and `NDILIB_LIBRARY_NAME` as `Processing.NDI.Lib.x64.dll` on 64-bit Windows. |
| S6 | DistroAV (the OBS NDI plugin), https://github.com/DistroAV/DistroAV | Prior art for an open-source app: it requires the user to install the NDI runtime rather than shipping it, and loads it dynamically. OBS users who want NDI already have the runtime for this reason. |
| S7 | NDI 6 Runtime redistributable, http://ndi.link/NDIRedistV6 → https://downloads.ndi.tv/SDK/NDI_SDK/NDI%206%20Runtime.exe (fetched 2026-10-07: 9,648,232 bytes, Last-Modified 2026-04-16, Inno Setup, Authenticode "Vizrt AG", FileVersion 6.3.2.0) | Contains `app\Processing.NDI.Lib.x64.dll` (6.3.2.0, signed "Vizrt AG", "Copyright (C) 2023-2026 Vizrt NDI AB. All rights reserved."), `app\Processing.NDI.Lib.Licenses.txt`, the licence PDF, and x86/UWP/DirectShow variants Relay does not ship. The notice file says: "This file should be included with all distribution of the binary files included with the NDI SDK." |
| S8 | innoextract 1.9, https://constexpr.org/innoextract/ (zlib licence; release zip from GitHub, SHA-256 pinned) | Unpacks Inno Setup installers without running them. Build-time tool only; not shipped. |

## How each obligation is met

| Obligation | Where |
|---|---|
| Distribute only object code the documentation allows (S1 §2a, §2d; S3) | Only the runtime DLL from NDI's redistributable, plus the notice file NDI asks to accompany it (S7). No header, import lib, SDK file or other variant. |
| EULA carrying S1 §3d's terms (S1 §3d; S3) | Installer licence page, Part 2: no modification, no reverse engineering/disassembly/recompilation (protocols included), no circumvention, no removal of notices, warranty and liability disclaimers for NDI and its licensors, US export compliance, NDI's copyright notice, and the pass-through for developers building on Relay. Part 2 is scoped to the NDI files, so Relay's own code stays MIT. |
| Copyright notices (S1 §3d(vii), §3g) | Installer licence page, `THIRD_PARTY_NOTICES.md` §4, About card, and NDI's notice file installed beside the DLL. |
| Keep it current (S1 §2b; S3) | The pins (`scripts/ndi-runtime.psd1`) match NDI's unversioned redistributable URL, so when NDI ships a new runtime the release build fails until the pins are bumped. See "Updating the bundled runtime". |
| DLL in the app's folder, not a system path (S2) | Tauri resource `binaries/ndi` → the install folder. The Tauri uninstaller removes both files. |
| Trademark use and links (S1 §3f; S2) | Unchanged from S51: "NDI®" on first use, the trademark line and an ndi.video link next to every NDI toggle and in the About card; not endorsed by Vizrt NDI AB; no NDI in the product name. |
| Interoperability (S1 §3h) | The runtime is NDI's own, unmodified. |
| Product type (S1 §1b) | Relay is desktop software on a general-purpose OS. |
| Codecs (S2) | Relay hands the runtime uncompressed frames and audio; it sends no AAC, H.264 or H.265 over NDI. |

## The fetch, step by step (`scripts/fetch-ndi-runtime.ps1`)

1. Download the redistributable from the pinned URL (the target of
   http://ndi.link/NDIRedistV6), check its SHA-256 and that it is
   Authenticode-valid and signed by `CN=Vizrt AG, O=Vizrt AG`.
2. Download innoextract 1.9 (pinned SHA-256) and unpack the installer. The
   installer is never run, so the build machine gets no NDI install and no
   `NDI_RUNTIME_DIR_V6`.
3. Check each shipped file's SHA-256, the DLL's signature, and its
   FileVersion against the pin.
4. Copy to the build cache folder. `stage-bundle.ps1` copies from
   there into `ui\src-tauri\binaries\ndi`, re-checks hashes and signature,
   and refuses any `Processing.NDI.*` file anywhere else in the staging folder.

SignPath does not re-sign the DLL: it keeps Vizrt's signature, and the
release workflow only sends Relay's own binaries for signing.

## Updating the bundled runtime

Part of the release checklist (S1 §2b). When the release build fails with
"NDI 6 Runtime installer SHA-256 mismatch":

1. Download http://ndi.link/NDIRedistV6 by hand; confirm the signature
   (`Get-AuthenticodeSignature`) is Valid and from Vizrt.
2. Read NDI's licensing and distribution pages (S1–S3) again for changes.
3. Unpack it with innoextract; record the installer's and the two files'
   SHA-256s and the DLL's FileVersion in `scripts/ndi-runtime.psd1`.
4. Update the version in this file, `THIRD_PARTY_NOTICES.md` §4, and the
   installer licence page if the copyright years changed.
5. Run the NDI tests on two PCs (`docs/plans/S51-ndi-output.md`) before
   tagging.

A runtime that moves to a new major version (NDI 7) also needs the FFI
layouts re-checked and the `NDI_RUNTIME_DIR_V6` fallback renamed.

## LAN only

NDI discovers sources with mDNS on the local network by default, which
matches Relay's LAN-only rule. Relay creates its sender with no groups and
does not configure an NDI Discovery Server or NDI Bridge. If a user has set
up a discovery server in NDI Access Manager, that is their NDI
configuration; Relay neither changes nor overrides it (`NDI_CONFIG_DIR` is
left alone, per "never touch global config"). Bundling changes nothing here:
the bundled runtime reads the same per-user NDI configuration an installed
one would.

## History

- S51 (v1): not bundled; the user installed the runtime from NDI. The
  reasoning then was that bundling adds permanent licence and release
  duties (EULA page, keeping current, a release-time download).
- RC2 (2026-10-07): owner chose to take those duties on so NDI output needs
  no extra install. This document records how each is met.
