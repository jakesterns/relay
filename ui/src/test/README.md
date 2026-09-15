# UI test harness

`pnpm test` in `ui/`. Vitest + jsdom + Testing Library, run in CI on
`windows-latest` next to `pnpm build`.

## The hard rule

**No test moves the real mouse or sends synthetic keystrokes to the desktop.**
An earlier attempt drove a real Tauri window over WebDriver; it moved the
actual cursor and its clicks landed in the user's browser. That is why this
suite is component tests, not WebDriver:

| | component tests (chosen) | WebDriver + tauri-driver |
|---|---|---|
| Input | DOM events inside a jsdom document in this process | real `SendInput` at the OS cursor |
| Needs a window | no | yes, and it takes focus |
| Runs in CI | yes, ~10 s | needs a desktop session and the WebView2 driver |
| Fidelity | React tree, no layout, no real WebView | the shipping binary |

The fidelity gap is real and worth naming: these tests do not prove the Tauri
shell boots, that `ui/dist` was embedded, or that the WebView renders. Those
are covered elsewhere — `pnpm build` compiles and type-checks the same
sources, `cargo test -p relay-core` covers the IPC wire shapes on both sides,
and `crash_restore.rs` drives a real core over the pipe. What had no coverage
at all was the screens, and that is what this fills.

`safety.test.ts` enforces the rule mechanically: it fails if a WebDriver or
input-synthesis package appears in `package.json`, or if the Tauri aliases stop
pointing at the fake.

## The three modes

`src/lib/ipc.ts` reaches the desktop in exactly three places — dynamic imports
of `@tauri-apps/api/{core,event,window}`. `vitest.config.ts` aliases all three
to `src/test/tauriMock.ts`, so a test picks the situation it wants:

```ts
tauri.useMockData();              // browser: ipc.ts serves its own mock data
tauri.useOfflineCore();           // inside Tauri, every invoke rejects
tauri.useFakeCore(core.handler);  // a scripted core answers
```

`makeFakeCore()` (`src/test/fakeCore.ts`) implements every command in
`ipc.ts` with the shapes that file declares, over mutable state — so a test
can click Save and then assert the core holds the new value. Set
`core.fail.set("install_apo", "…")` to make one command reject with the
message the real core would give.

Events are pushed with `tauri.emit("core://share-stats", {…})` wrapped in
`push()`.

## Writing a test

```ts
const h = renderScreen(<Settings />);   // real CoreProvider around it
await settle();                          // let the mount's promises land
await h.user.click(screen.getByRole("button", { name: "Install…" }));
expect(tauri.lastCall("install_apo")?.args).toEqual({ … });
h.expectClean();                         // nothing thrown, nothing console.error'd
```

`src/test/dom.ts` has queries for the shapes this design uses instead of
labelled form controls: `card()`, `inCard()`, `field()`, `kv()`, `slider()`,
`readout()` (instrument strip), `monoLines()`.

Vitest isolates each test file, so the module-level mock stores inside
`ipc.ts` cannot leak between files. They *can* leak between tests in one
file — keep mock-mode tests read-only, or use the fake core, which is rebuilt
in `beforeEach`.

## Coverage

- `screens.smoke.test.tsx` — every screen × mock data / offline core / live
  core: renders, throws nothing, logs nothing, and says something true about
  the mode. Also checks that every command a screen issues is one the core
  implements.
- `App.test.tsx` — first-run gate, rail navigation, the "nothing was changed"
  line, title-bar window controls.
- `Share.test.tsx` — preset start/stop with its arguments, the preset editor,
  recording/replay, source switching, the instrument strip.
- `Profiles.test.tsx` — catalogue search and import, profile CRUD, hardware
  library.
- `Settings.test.tsx` — uninstall plan rendering, the APO and virtual-camera
  opt-ins, autostart, recording settings.
- `FirstRun.test.tsx` — the consent flow, and that consenting installs nothing.
- `Games.test.tsx` — EQ/limiter/display serialisation into a profile.
- `Receive.test.tsx` — pairing, and whether a call will see the stream.
