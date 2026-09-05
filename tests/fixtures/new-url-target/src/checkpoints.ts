// Three outbound requests from one client, all of the same shape: the target
// is a `new URL(path, base)` whose base is a member of an options object the
// constructor took, and the request is written across several lines with its
// verb on the options bag.
//
// The base is opaque. The path is the only thing the source states about the
// route, so a base the model reports in front of that path is a paraphrase of
// the receiver, not a reading of the source — except when it is a literal
// absolute origin, which is a thing the model can only have read.
export class CheckpointClient {
  constructor(private readonly opts: { apiUrl: string; orchestrator: string }) {}

  // The model invents an alias for the opaque base.
  async suspendRun(runFriendlyId: string, snapshotFriendlyId: string): Promise<Response> {
    return fetch(
      new URL(
        `/api/v1/runs/${runFriendlyId}/snapshots/${snapshotFriendlyId}/suspend`,
        this.opts.apiUrl
      ),
      {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ type: this.opts.orchestrator }),
      }
    );
  }

  // The model spells the receiver back as the base.
  async continueRun(runFriendlyId: string, snapshotFriendlyId: string): Promise<Response> {
    return fetch(
      new URL(
        `/api/v1/runs/${runFriendlyId}/snapshots/${snapshotFriendlyId}/continue`,
        this.opts.apiUrl
      ),
      {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ type: this.opts.orchestrator }),
      }
    );
  }

  // The model reports a literal absolute origin: the one prefix it can have
  // read rather than paraphrased, and the one that classifies the call.
  async completeAttempt(runFriendlyId: string, attemptFriendlyId: string): Promise<Response> {
    return fetch(
      new URL(
        `/api/v1/runs/${runFriendlyId}/attempts/${attemptFriendlyId}/complete`,
        this.opts.apiUrl
      ),
      {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ type: this.opts.orchestrator }),
      }
    );
  }
}
