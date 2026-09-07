export declare class RunClient {
  subscribeToRun(runId: string): AsyncIterable<unknown>;
  fetchStream(runId: string, streamKey: string): AsyncIterable<unknown>;
}
