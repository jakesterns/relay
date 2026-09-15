/**
 * Render helpers.
 *
 * `userEvent` here is the Testing Library one: it dispatches DOM events at
 * elements inside this jsdom document. It has no connection to the host's
 * input stack — no `SendInput`, no cursor, no focus stealing. That is the
 * whole reason this suite is component tests rather than WebDriver against a
 * real Tauri window.
 */
import { Component, StrictMode, type ReactNode } from "react";
import { act, render, type RenderResult } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { expect, vi } from "vitest";
import App from "../App";
import { CoreProvider } from "../lib/core";

/** Catches anything a screen throws during render so the assertion can name
 *  it, instead of the test dying inside React with a stack and no context. */
class Boundary extends Component<{ sink: Error[]; children: ReactNode }, { dead: boolean }> {
  state = { dead: false };
  static getDerivedStateFromError() {
    return { dead: true };
  }
  componentDidCatch(error: Error) {
    this.props.sink.push(error);
  }
  render() {
    return this.state.dead ? null : this.props.children;
  }
}

export interface Harness extends RenderResult {
  user: ReturnType<typeof userEvent.setup>;
  /** Errors a render threw, collected by the boundary. */
  thrown: Error[];
  /** Anything React or the app logged at error level. */
  consoleErrors: unknown[][];
  /** Assert the render was clean: nothing thrown, nothing logged as an error. */
  expectClean: () => void;
}

function harness(ui: ReactNode, strict: boolean): Harness {
  const thrown: Error[] = [];
  const consoleErrors: unknown[][] = [];
  const spy = vi.spyOn(console, "error").mockImplementation((...args: unknown[]) => {
    consoleErrors.push(args);
  });

  const tree = <Boundary sink={thrown}>{ui}</Boundary>;
  const result = render(strict ? <StrictMode>{tree}</StrictMode> : tree);

  return {
    ...result,
    user: userEvent.setup({ document }),
    thrown,
    consoleErrors,
    expectClean() {
      spy.mockRestore();
      expect(thrown.map((e) => e.message)).toEqual([]);
      expect(consoleErrors.map((a) => String(a[0]))).toEqual([]);
    },
  };
}

/** One screen, inside the real `CoreProvider` (so it polls and subscribes
 *  exactly as it does in the app). */
export function renderScreen(ui: ReactNode, opts: { strict?: boolean } = {}): Harness {
  return harness(<CoreProvider>{ui}</CoreProvider>, opts.strict ?? false);
}

/** The whole shell, including the title bar, rail and first-run gate. */
export function renderApp(opts: { strict?: boolean } = {}): Harness {
  return harness(<App />, opts.strict ?? false);
}

/** Let every promise the mount kicked off settle. */
export async function settle(times = 3) {
  for (let i = 0; i < times; i++) {
    await act(async () => {
      await Promise.resolve();
    });
  }
}

/** Push a core event and let React apply it. */
export async function push(emit: () => void) {
  await act(async () => {
    emit();
    await Promise.resolve();
  });
}
