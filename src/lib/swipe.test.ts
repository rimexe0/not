import { describe, expect, it, vi } from "vitest";
import { SwipeTracker } from "./swipe";

describe("SwipeTracker", () => {
  it("ignores vertical and diagonal scrolling", () => {
    const swipe = new SwipeTracker();
    expect(swipe.push(20, 30)).toEqual({ direction: 0, distance: 0 });
    expect(swipe.push(20, 18)).toEqual({ direction: 0, distance: 0 });
  });

  it("tracks gesture distance without navigating mid-gesture", () => {
    const swipe = new SwipeTracker();
    expect(swipe.push(30, 2)).toEqual({ direction: 1, distance: 30 });
    expect(swipe.push(45, 2)).toEqual({ direction: 1, distance: 75 });
    expect(swipe.finish(70)).toBe(1);
  });

  it("cancels a gesture that ends before the threshold", () => {
    const swipe = new SwipeTracker();
    swipe.push(-40, 0);
    expect(swipe.finish(70)).toBe(0);
  });

  it("allows direction reversal within the same gesture", () => {
    const swipe = new SwipeTracker();
    swipe.push(80, 0);
    expect(swipe.push(-120, 0)).toEqual({ direction: -1, distance: 40 });
    expect(swipe.finish(30)).toBe(-1);
  });

  it("retains the final gesture velocity for animation handoff", () => {
    const now = vi.spyOn(performance, "now");
    now.mockReturnValueOnce(100).mockReturnValueOnce(110).mockReturnValue(120);
    const swipe = new SwipeTracker();
    swipe.push(10, 0);
    swipe.push(10, 0);
    expect(swipe.completionVelocity(1)).toBeCloseTo(0.45);
    expect(swipe.completionVelocity(-1)).toBe(0);
    now.mockRestore();
  });

  it("absorbs same-direction momentum after navigation", () => {
    vi.useFakeTimers();
    const swipe = new SwipeTracker(100);
    swipe.suppressMomentum(1);
    expect(swipe.push(80, 0)).toEqual({ direction: 0, distance: 0 });
    vi.advanceTimersByTime(101);
    expect(swipe.push(80, 0)).toEqual({ direction: 1, distance: 80 });
    swipe.reset();
    vi.useRealTimers();
  });

  it("allows an opposite gesture through the momentum guard immediately", () => {
    const swipe = new SwipeTracker();
    swipe.suppressMomentum(1);
    expect(swipe.push(-80, 0)).toEqual({ direction: -1, distance: 80 });
    swipe.reset();
  });
});
