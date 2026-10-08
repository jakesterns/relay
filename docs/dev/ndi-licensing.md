# NDI® licensing for Relay (S51)

NDI® is a registered trademark of Vizrt NDI AB. More about NDI: https://ndi.video/

Read 2026-10-07. This is an engineering reading of the published terms, not
legal advice. Anything marked **owner's call** is a decision for Jake, and
bundling (option B below) is one to put past a lawyer first.

## Verdict

1. **Relay can load the NDI runtime dynamically, and does.** `relay-share`
   calls `LoadLibraryExW` on the full path of `Processing.NDI.Lib.x64.dll`
   and `GetProcAddress` on each of the C functions it uses. The FFI
   declarations are written by hand in `crates/capture/src/ndi/ffi.rs`. No NDI
   SDK header, library, import lib or binary is committed to the repository,
   and none is needed to build Relay.
2. **Relay does not ship the NDI runtime DLL or installer (v1).** The user
   installs the free *NDI 6 Runtime* from NDI (http://ndi.link/NDIRedistV6, or
   NDI Tools from https://ndi.video/tools/, which includes it). Relay finds it
   through the `NDI_RUNTIME_DIR_V6` environment variable that installer sets,
   or in its default folder (`%ProgramFiles%\NDI\NDI 6 Runtime\v6`) when Relay
   was already running at install time and so has no such variable yet.
   Without it, the NDI output toggle says "NDI output needs the NDI runtime"
   and links the download; nothing crashes and nothing else changes.
3. **Bundling is allowed by NDI's documentation but carries obligations Relay
   does not meet today** (an end-user licence carrying NDI's terms, keeping the
   bundled copy current, and a release build that fetches the binary without
   it ever entering the public repository). Whether to take that on is the
   **owner's call**; see option B.
4. **Notices:** trademark line and an https://ndi.video/ link next to every
   NDI toggle, the trademark line in the About card (Settings), a section in
   `THIRD_PARTY_NOTICES.md`, and the same in the docs. No "NDI" in the product
   name. Details under "What goes where".

## Sources

| # | Source | What it says (quoted or closely paraphrased) |
|---|---|---|
| S1 | NDI SDK License Agreement, https://downloads.ndi.tv/SDK/NDI_SDK/NDI%20SDK%20License%20Agreement.pdf (PDF dated 2025-08-21; also linked as http://ndi.link/ndisdk_license) | §2a royalty-free licence "to distribute, only in accordance with the SDK Documentation requirements, object code included in the SDK solely as used by such Products". §2b "The Bundled Product must incorporate and be compatible with the latest version of the SDK available at the time of such use, development, or distribution". §2d "Unless otherwise stated in the SDK, no files within the SDK and the Specific SDK may be distributed. Certain files ... may be distributed, said files and their respective distribution license are individually identified within the SDK documentation." §3d any distribution of your Product must be "under the terms of a license agreement containing terms that: (i) prohibit any modifications to the SDK ... (ii) prohibit any reverse engineering, disassembly or recompilation ... (iii) prohibit any circumvention of any technical limitations ... (iv) [prohibit] removal ... of any proprietary notices", disclaim warranties and liability on NDI's behalf, comply with US export law, and "include the appropriate copyright notice showing NDI ... as copyright owner". §3f NDI trademarks only "to identify that the Product is compatible with NDI Products, and in all cases ... special and clear notations shall be provided that the marks are NDI trademarks"; no suggestion of sponsorship. §3h Products must stay interoperable with other NDI products. §1b "Products" are software for general-purpose computers the user can freely change (desktops qualify; locked appliances and embedded devices need a commercial licence). Governing law: Sweden. |
| S2 | NDI docs, *Licensing*, https://docs.ndi.video/all/developing-with-ndi/sdk/licensing | Provide "a link to ndi.video in a location close to all locations where NDI is used/selected within the product, on your website, and in its documentation." Use NDI only as "NDI®" with "NDI® is a registered trademark of Vizrt NDI AB" on the same page near the first use or in a footnote; the About box must carry the same statement. Do not distribute NDI Tools; link to https://ndi.video/tools/. "You should include the NDI DLLs as part of your own application and keep them in your application folders" — never in system paths. Contact NDI before using "NDI" in a product name. Codec (AAC/H.264/H.265) licensing is the developer's responsibility. |
| S3 | NDI docs, *Software Distribution*, https://docs.ndi.video/all/developing-with-ndi/sdk/software-distribution | Header files "may be distributed with open-source projects under the terms of the MIT license". Binaries are distributable within applications provided the application's EULA covers the NDI SDK requirements. The redistributable installers may be included in your installer (silently, `/verysilent`) provided the licence terms are covered in your documentation and you keep them current; or "provide a user link to the NDI-provided download". |
| S4 | NDI docs, *Dynamic Loading of NDI Libraries*, https://docs.ndi.video/all/developing-with-ndi/sdk/dynamic-loading-of-ndi-libraries | Dynamic loading is a supported way to use NDI. The runtime may sit in the application folder, or "on Windows, you can install the NDI runtime and use an environment variable to locate it on disk"; `NDILIB_REDIST_URL` is the download location, e.g. http://ndi.link/NDIRedistV6. |
| S5 | NDI SDK header `Processing.NDI.Lib.h` (as published in open-source projects, e.g. https://github.com/DistroAV/DistroAV/blob/master/lib/ndi/Processing.NDI.Lib.h) | MIT licence text in the header; defines `NDILIB_REDIST_FOLDER` as the `NDI_RUNTIME_DIR_V6` environment variable and `NDILIB_LIBRARY_NAME` as `Processing.NDI.Lib.x64.dll` on 64-bit Windows. |
| S6 | DistroAV (the OBS NDI plugin), https://github.com/DistroAV/DistroAV | Prior art for an open-source app: it requires the user to install the NDI runtime rather than shipping it, and loads it dynamically. OBS users who want NDI already have the runtime for this reason. |

## Reasoning

**Dynamic loading with hand-written declarations (allowed).** S4 documents
dynamic loading as a supported integration. Relay vendors nothing: the struct
layouts and function signatures in `ffi.rs` are written by hand from the
documented C API (S3, S5). Those headers are MIT-licensed, so even a
declaration that mirrors them closely is redistributable in an MIT project;
`THIRD_PARTY_NOTICES.md` §4 credits them anyway. Nothing from the SDK's
"Confidential Information" (S1 §3a: the SDK package, its tools, samples and
documentation beyond the public pages) is copied. Relay only loads the DLL
from a full path (the runtime folder named by `NDI_RUNTIME_DIR_V6`, the
runtime's default folder, or an explicit `RELAY_NDI_RUNTIME` override for
tests); it never searches `PATH`,
so a stray copy cannot be planted in its way.

**Not bundling (v1).** Bundling the DLL or the redistributable installer is
permitted by S2/S3, but each of these is a real obligation:

- *An end-user licence with NDI's terms (S1 §3d, S3).* Relay is distributed
  under MIT with no EULA. MIT permits modification and reverse engineering of
  *Relay*; that is fine, but the installer would also have to present terms
  that forbid modifying or reverse-engineering the *NDI* component, disclaim
  NDI's warranties and liability, and carry NDI's copyright notice. That means
  adding an NDI licence page to the NSIS installer and to `licenses.html`, and
  telling users the bundle is not entirely MIT.
- *Keep it current (S1 §2b, S3).* Each NDI SDK release would oblige a Relay
  release with the new runtime.
- *Getting the file into a release without the public repo.* The DLL cannot be
  committed (S1 §2d covers "files within the SDK"; the SDK package is not
  public). CI would have to download the redistributable from NDI at release
  time and verify it, which is a new outbound dependency for every release
  build.
- *DLL placement (S2).* It would have to live in Relay's own folder
  (`%LOCALAPPDATA%\Relay`), never in a system path.

None of that is impossible, but it trades a one-time runtime install, which
most people who use NDI already have (OBS's DistroAV needs the same runtime,
S6), for permanent licence and release duties. Relay's "zero setup" rule is
about Relay's own features; NDI output is an opt-in bridge to other NDI
software that already requires the runtime. So v1 links the runtime and
does not ship it.

**LAN only.** NDI discovers sources with mDNS on the local network by
default, which matches Relay's LAN-only rule. Relay creates its sender with no
groups and does not configure an NDI Discovery Server or NDI Bridge. If a user
has set up a discovery server in NDI Access Manager, that is their NDI
configuration, applied by the runtime they installed; Relay neither changes
nor overrides it (`NDI_CONFIG_DIR` is left alone, per "never touch global
config"). The two-PC plan checks with a capture that nothing leaves the LAN.

## What goes where

| Place | Text |
|---|---|
| Receive screen, NDI output card (and the Share screen's) | "NDI® output" as the first use; a footnote "NDI® is a registered trademark of Vizrt NDI AB" and a link to https://ndi.video/ in the same card. When the runtime is missing: "NDI output needs the NDI runtime" + link to http://ndi.link/NDIRedistV6. |
| Settings, About card | "NDI® is a registered trademark of Vizrt NDI AB" with the https://ndi.video/ link. |
| `THIRD_PARTY_NOTICES.md` §4 | What Relay uses (dynamic loading, the user-installed runtime), the trademark line, the link, that no NDI file is shipped, and the MIT credit for the header-derived declarations. |
| `README.md`, `docs/plans/S51-ndi-output.md` | Trademark line and link where NDI is first mentioned. |
| Product name | Relay never puts "NDI" in its name. "NDI output" labels a compatibility feature, which S1 §3f allows. The published source name is "Relay (from <sender>)" — no NDI in it. |

The `licenses.html` page (`scripts/licenses.ps1`, cargo-about) is untouched: it
lists the Rust and npm dependencies Relay actually ships, and no NDI code is
among them. `scripts/stage-bundle.ps1` refuses to stage an NDI binary, so one
cannot reach an installer by accident before option B is chosen.

## Option B: bundle later (owner's call)

If Jake wants NDI output to work with no runtime install:

1. Add an NDI component licence (the NDI end-user terms, or Relay terms
   carrying S1 §3d) as a license page in `ui/src-tauri/installer/hooks.nsh`
   and as a section of `licenses.html`.
2. Release CI downloads the NDI 6 redistributable from NDI, checks its
   signature (Vizrt NDI AB Authenticode), and either installs it silently from
   the Relay installer or stages the DLL next to `relay-share.exe`.
3. `ndi::locate_runtime` already checks for a DLL next to the engine *after*
   the user-installed runtime (step 2 would make that path real; today it is
   only reached by a developer who drops a DLL there by hand).
4. Track NDI SDK releases (S1 §2b) as part of the release checklist.
