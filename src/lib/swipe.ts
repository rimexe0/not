export interface SwipeUpdate {
  direction: -1 | 0 | 1;
  distance: number;
}

export class SwipeTracker {
  private accumulated = 0;
  private horizontalVelocity = 0;
  private lastHorizontalEventAt = 0;
  private suppressedDirection: -1 | 0 | 1 = 0;
  private suppressionTimer: ReturnType<typeof setTimeout> | undefined;

  constructor(private readonly suppressionMs = 140) {}

  push(deltaX: number, deltaY: number): SwipeUpdate {
    if (Math.abs(deltaX) <= Math.abs(deltaY) * 1.2) {
      return { direction: 0, distance: 0 };
    }

    const incomingDirection = deltaX > 0 ? 1 : -1;
    if (this.suppressedDirection !== 0) {
      if (incomingDirection === this.suppressedDirection || Math.abs(deltaX) < 4) {
        this.scheduleSuppressionEnd();
        return { direction: 0, distance: 0 };
      }
      this.clearSuppression();
      this.horizontalVelocity = 0;
      this.lastHorizontalEventAt = 0;
    }

    const now = performance.now();
    const elapsed = now - this.lastHorizontalEventAt;
    const sampleVelocity = elapsed > 0 && elapsed < 80 ? deltaX / elapsed : 0;
    this.horizontalVelocity = this.lastHorizontalEventAt === 0
      ? 0
      : this.horizontalVelocity * 0.55 + sampleVelocity * 0.45;
    this.lastHorizontalEventAt = now;
    this.accumulated += deltaX;
    return this.current();
  }

  finish(threshold: number): -1 | 0 | 1 {
    const direction = Math.abs(this.accumulated) >= threshold
      ? this.accumulated > 0 ? 1 : -1
      : 0;
    this.accumulated = 0;
    return direction;
  }

  suppressMomentum(direction: -1 | 1): void {
    this.accumulated = 0;
    this.suppressedDirection = direction;
    this.scheduleSuppressionEnd();
  }

  reset(): void {
    this.accumulated = 0;
    this.horizontalVelocity = 0;
    this.lastHorizontalEventAt = 0;
    this.clearSuppression();
  }

  completionVelocity(direction: -1 | 1): number {
    return performance.now() - this.lastHorizontalEventAt < 100
      && Math.sign(this.horizontalVelocity) === direction
      ? Math.abs(this.horizontalVelocity)
      : 0;
  }

  private scheduleSuppressionEnd(): void {
    clearTimeout(this.suppressionTimer);
    this.suppressionTimer = setTimeout(() => this.clearSuppression(), this.suppressionMs);
  }

  private clearSuppression(): void {
    this.suppressedDirection = 0;
    clearTimeout(this.suppressionTimer);
    this.suppressionTimer = undefined;
  }

  private current(): SwipeUpdate {
    return {
      direction: this.accumulated === 0 ? 0 : this.accumulated > 0 ? 1 : -1,
      distance: Math.abs(this.accumulated),
    };
  }
}
