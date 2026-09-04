import { mergeOptions, send } from "./send.js";

const ReportSchema = { kind: "report" };

// The other half of `list`: a second module on the same surface declaring the
// name with a different request. A site calling `.list()` could be reaching
// either, so it must reach neither.
export class ReportClient {
  constructor(private readonly baseUrl: string) {}

  list() {
    return send(
      ReportSchema,
      `${this.baseUrl}/api/v2/reports`,
      {
        method: "GET",
        headers: { Accept: "application/json" },
      },
      mergeOptions({}, {})
    );
  }
}
