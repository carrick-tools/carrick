import type { RunEvent } from "../client/index.js";

/** A second class on the same published surface, declaring `fetchStream` too. */
export class BatchClient {
  fetchStream(runId: string, streamKey: string): AsyncIterable<RunEvent> {
    throw new Error(`not implemented: ${runId}/${streamKey}`);
  }
}
