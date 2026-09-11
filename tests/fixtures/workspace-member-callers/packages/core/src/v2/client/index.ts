export type RunEvent = {
  id: string;
  status: string;
};

export class RunClient {
  constructor(private readonly baseUrl: string) {}

  subscribeToRun(runId: string): AsyncIterable<RunEvent> {
    return this.open(`${this.baseUrl}/runs/${runId}/subscribe`);
  }

  fetchStream(runId: string, streamKey: string): AsyncIterable<RunEvent> {
    return this.open(`${this.baseUrl}/runs/${runId}/streams/${streamKey}`);
  }

  private open(url: string): AsyncIterable<RunEvent> {
    throw new Error(`not implemented: ${url}`);
  }
}

/**
 * The factory a consumer calls once, at import time, to get its client — the
 * shape a module-scope `const` reaches for.
 */
export function createRunClient(baseUrl: string): RunClient {
  return new RunClient(baseUrl);
}
