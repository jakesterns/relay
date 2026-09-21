# Reproducible builds (B12)

What "the same commit gives the same bytes" means for Relay today, what was
measured, and how to reason about a hash mismatch. Written in S33
(2026-09-18); numbers are from `ebc0c72` on the dev box, rustc stable, MSVC
Build Tools 2022.

## What was measured

`scripts/repro-check.ps1` adds a throwaway worktree of `HEAD` next to the repo,
builds the same packages in release in both folders (separate target dirs,
nothing shared), hashes every `.exe`/`.dll`, and removes the worktree.

```
powershell -ExecutionPolicy Bypass -File scripts\repro-check.ps1 -Packages relay-core,relay-capture
powershell -ExecutionPolicy Bypass -File scripts\repro-check.ps1 -Packages relay-core,relay-capture -Flags
```

| build | relay-core | relay-svc | relay-elevate | relay-share |
|---|---|---|---|---|
| plain `cargo build --release`, two folders | differ | differ | differ | differ |
| with `scripts/repro-flags.ps1`, two folders | **identical** | **identical** | **identical** | **identical** |

`relay-share.exe` is the hard one — it links libopus built by cmake/MSVC
through `opusic-sys` — and it came out identical too (the `cc`/`cmake` crates
already pass `-Brepro` to the C compiler).

## The inputs that varied, and what fixes each

1. **Where the repo lives.** rustc bakes absolute source paths into every
   binary: panic locations, `file!()`, the cargo registry path of every
   dependency. Two worktrees are two paths, so *every* Rust binary differed.
   Fixed by `--remap-path-prefix` for the repo root (`relay`), the cargo home
   (`cargo`) and the rustup home (`rustup`). Side effect worth knowing: panic
   messages and logs now say `relay\crates\core\src\...` rather than a full
   path.
2. **When the linker ran.** MSVC `link.exe` writes a timestamp into the PE
   header and the debug directory. Fixed by `/Brepro`
   (`-Clink-arg=/Brepro`), which replaces it with a hash of the content.
   This, not caching as such, was the `relay-preview.exe` finding: cargo kept
   an older link of identical source in one tree and relinked in the other,
   and two links were never the same bytes.
3. **`autoeq-index.tsv` line endings.** `.gitattributes` says `eol=lf`, but a
   working tree checked out before that file existed keeps its CRLF copy, and
   git does not report it as modified (it normalises on compare). Measured
   2026-09-18: the main checkout (`Stream Share`) has 8,849 CRLFs, 768,682
   bytes, `50b8ae6d...`; every later worktree has LF, 759,833 bytes,
   `3d7ef410...`. `stage-bundle.ps1` now writes the staged copy with LF
   whatever the tree has. (`git add --renormalize .` would not help: the index
   is already LF, it is the working copy that is stale. Re-checking the file
   out fixes that one tree; staging fixes all of them.)

The flags are passed as `CARGO_ENCODED_RUSTFLAGS` (0x1f-separated), not
`RUSTFLAGS`, because the main checkout's path contains a space.
`stage-bundle.ps1` sets them for its own cargo builds *and leaves them set*,
so the `pnpm tauri build` that `install-local.ps1` runs next in the same
process builds `relay-ui.exe` with them too. Run `tauri build` from a fresh
shell and `relay-ui.exe` is built without them.

Cost: cargo treats a change of flags as a different build, so alternating
between `stage-bundle.ps1` and a plain `cargo build --release` in one tree
rebuilds everything each time. Debug builds and tests are unaffected.

## What is *not* yet shown to be reproducible

Be precise about this when a hash does not match:

- **`relay-ui.exe`, `relay-preview.exe`, `relay_apo.dll`,
  `relay_vdevice.dll`.** Same compiler, same flags, so the three causes above
  are covered, but they were not in the measured set. `relay-ui.exe` has two
  extra inputs: the Vite output embedded by `tauri::generate_context!`
  (content-hashed file names; depends on `pnpm-lock.yaml` being honoured, so
  install with `--frozen-lockfile`), and the Windows resource section
  `tauri-build` generates.
- **The loose `relay-ui.exe` is not the shipped one.** `tauri build` rewrites
  `target\release\relay-ui.exe` after it has written the installer (S29 saw
  12 ms between them, and different hashes). Hash the installer, or the exe
  extracted from it; never the loose file.
- **The NSIS installer itself.** `makensis` stores each file's modification
  time and compresses the payload as one solid LZMA stream, so two installers
  built from byte-identical inputs at different times still differ. Making it
  reproducible needs either fixed mtimes on the staged files plus a pinned
  NSIS, or comparing installers by *contents* (extract with 7-Zip and hash the
  files), which is the practical check today.
- **The toolchain.** None of this pins rustc, the MSVC linker, cmake, NSIS or
  the Windows SDK. A different rustc gives different code; that is a version
  difference, not non-determinism, and `rustc -vV`, `link.exe` and
  `makensis /VERSION` belong in the release notes of anything that gets
  signed. There is no `rust-toolchain.toml` yet; adding one is the cheap half
  of this.
- **Signing.** An Authenticode signature embeds a timestamp countersignature,
  so signed files never match; compare before signing, or strip the signature.

## Reasoning about a mismatch

In order, cheapest first:

1. Same commit, clean tree? (`git status`, and `Cargo.lock` unchanged.)
2. Same toolchain? (`rustc -vV`; MSVC and SDK versions.)
3. Was it built through `stage-bundle.ps1`? A plain `cargo build --release`
   carries paths and a link timestamp — every Rust binary will differ, and
   differ again on every relink.
4. Only `autoeq-index.tsv` differs by exactly 8,849 bytes: a CRLF copy was
   staged by something other than `stage-bundle.ps1`.
5. Only `relay-ui.exe` differs: comparing the loose file rather than the
   installed one, or a different `ui/dist` (check `pnpm-lock.yaml` and Node).
6. Only the installer differs while every extracted file matches: NSIS
   mtimes/compression, expected today.
7. Anything else is new — run `scripts\repro-check.ps1 -Flags -Packages <pkg>`
   on the crate that differs and add what you find here.
