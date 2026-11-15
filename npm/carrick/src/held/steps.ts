// Per-step timing for one held-compiler request. A step is a name and the
// milliseconds spent in it (one decimal); a name used twice adds up. The daemon
// writes the steps into its request log so a slow answer says where it went.

export type Steps = Record<string, number>;

export const tenth = (ms: number): number => Math.round(ms * 10) / 10;

export class StepTimer {
  private readonly total = new Map<string, number>();

  /** Run `fn`, add its elapsed time to `name`, and return what it returned. */
  async run<T>(name: string, fn: () => Promise<T> | T): Promise<T> {
    const started = performance.now();
    try {
      return await fn();
    } finally {
      this.add(name, performance.now() - started);
    }
  }

  add(name: string, ms: number): void {
    this.total.set(name, (this.total.get(name) ?? 0) + ms);
  }

  steps(): Steps {
    return Object.fromEntries([...this.total].map(([name, ms]) => [name, tenth(ms)]));
  }
}

/** The sum of two step sets, by name. */
export function addSteps(into: Steps, more: Steps): void {
  for (const [name, ms] of Object.entries(more)) into[name] = tenth((into[name] ?? 0) + ms);
}
