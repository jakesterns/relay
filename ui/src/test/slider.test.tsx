/**
 * Owner 2026-10-24: dragging the dB sliders felt stiff. While the pointer is
 * down the thumb follows at once and the caller hears at most one change per
 * frame; a late value from the core cannot pull the thumb back mid-drag.
 */
import { describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { Slider, parseTyped } from "../components/Controls";
import css from "../styles/shell.css?raw";

const range = () => screen.getByRole("slider") as HTMLInputElement;
const frame = () => act(() => new Promise<void>((r) => requestAnimationFrame(() => r())));

describe("Slider drag", () => {
  it("reports a keyboard or plain change at once", () => {
    const onChange = vi.fn();
    render(<Slider label="Footsteps" value={0} min={-24} max={12} step={0.1} onChange={onChange} />);
    fireEvent.change(range(), { target: { value: "3.4" } });
    expect(onChange).toHaveBeenCalledWith(3.4);
  });

  it("moves the thumb at once and reports once per frame while dragging", async () => {
    const onChange = vi.fn();
    const { container } = render(<Slider label="Footsteps" value={0} min={-24} max={12} step={0.1} onChange={onChange} />);
    fireEvent.pointerDown(range());
    fireEvent.change(range(), { target: { value: "2" } });
    fireEvent.change(range(), { target: { value: "5.5" } });
    expect(container.querySelector(".val")?.textContent).toBe("5.5");
    expect(onChange).not.toHaveBeenCalled();
    await frame();
    expect(onChange).toHaveBeenCalledTimes(1);
    expect(onChange).toHaveBeenLastCalledWith(5.5);
  });

  it("keeps the thumb where the pointer is when the value prop lags, and sends the last value on release", () => {
    const onChange = vi.fn();
    const { container, rerender } = render(<Slider label="Bands" value={0} min={-12} max={12} step={0.1} onChange={onChange} />);
    fireEvent.pointerDown(range());
    fireEvent.change(range(), { target: { value: "4" } });
    rerender(<Slider label="Bands" value={1} min={-12} max={12} step={0.1} onChange={onChange} />);
    expect(container.querySelector(".val")?.textContent).toBe("4");
    fireEvent.pointerUp(window);
    expect(onChange).toHaveBeenLastCalledWith(4);
  });
});

describe("Slider knob", () => {
  it("never takes the pointer, so pressing it grabs the input's thumb", () => {
    expect(css).toMatch(/\.sl \.tr:after \{[^}]*pointer-events: none/);
    expect(css).toMatch(/\.sl \.tr:before \{[^}]*pointer-events: none/);
    expect(css).toMatch(/::-webkit-slider-thumb \{[^}]*width: 12px/);
  });
});

describe("Slider typed value", () => {
  it("parses what people type", () => {
    expect(parseTyped("+3")).toBe(3);
    expect(parseTyped("−4.5 dB")).toBe(-4.5);
    expect(parseTyped("150%")).toBe(150);
    expect(parseTyped("loud")).toBeNull();
  });

  it("click the number, type, Enter: clamped and snapped to the step", () => {
    const onChange = vi.fn();
    render(<Slider label="Footsteps" value={2} min={-24} max={12} step={0.1}
      format={(v) => `${v} dB`} onChange={onChange} />);
    fireEvent.click(screen.getByRole("button", { name: /Footsteps/ }));
    const box = screen.getByRole("textbox", { name: /Footsteps/ });
    fireEvent.change(box, { target: { value: "9.37" } });
    fireEvent.keyDown(box, { key: "Enter" });
    expect(onChange).toHaveBeenLastCalledWith(9.4);
    fireEvent.click(screen.getByRole("button", { name: /Footsteps/ }));
    const again = screen.getByRole("textbox", { name: /Footsteps/ });
    fireEvent.change(again, { target: { value: "40" } });
    fireEvent.blur(again);
    expect(onChange).toHaveBeenLastCalledWith(12);
  });

  it("Escape or nonsense leaves the value alone", () => {
    const onChange = vi.fn();
    render(<Slider label="Gunshots" value={-6} min={-24} max={12} step={0.1} onChange={onChange} />);
    fireEvent.click(screen.getByRole("button", { name: /Gunshots/ }));
    const box = screen.getByRole("textbox", { name: /Gunshots/ });
    fireEvent.change(box, { target: { value: "-10" } });
    fireEvent.keyDown(box, { key: "Escape" });
    fireEvent.click(screen.getByRole("button", { name: /Gunshots/ }));
    fireEvent.change(screen.getByRole("textbox", { name: /Gunshots/ }), { target: { value: "abc" } });
    fireEvent.keyDown(screen.getByRole("textbox", { name: /Gunshots/ }), { key: "Enter" });
    expect(onChange).not.toHaveBeenCalled();
  });
});
