import { getRun, readRun, saveRun, transport, type Transport } from "@fixture/client";
import type { Run, SavedRun } from "./types";

/** What a view of a run shows. */
export interface RunView {
  title: string;
}

/** A run's state, named. */
export type RunState = string;

function toView(run: Run): RunView {
  return { title: run.id };
}

/** A client whose methods say what they return. */
export class TypedClient {
  constructor(private readonly transport: Transport) {}

  /** Reads one member of what the request answers, through the helper below. */
  getRun(id: string): Promise<Run> {
    return this.run<Run>(() => getRun({ client: this.transport, path: { id } }));
  }

  /** The same shape through a request whose declared type is a stale copy. */
  saveRun(id: string, state: string): Promise<SavedRun> {
    return this.run<SavedRun>(() =>
      saveRun({ client: this.transport, path: { id }, body: { state } }),
    );
  }

  /** A class member over a `fetch` that parses. */
  read(id: string): Promise<Run> {
    return readRun(id);
  }

  /** The value goes through another function before it is returned. */
  async getRunView(id: string): Promise<RunView> {
    return toView(await this.run<Run>(() => getRun({ client: this.transport, path: { id } })));
  }

  /** Two reads: the helper's member, then the run's own. */
  async getRunState(id: string): Promise<RunState> {
    const r = await this.run<Run>(() => getRun({ client: this.transport, path: { id } }));
    return r.state;
  }

  private async run<T>(call: () => Promise<{ data: unknown }>): Promise<T> {
    let result;
    try {
      result = await call();
    } catch (e) {
      throw new Error(String(e));
    }
    if (result.data === null) return undefined as T;
    return result.data as T;
  }
}

export function createTypedClient(): TypedClient {
  return new TypedClient(transport);
}
