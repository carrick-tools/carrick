import { transport, type RequestOptions } from "./transport";

/** What the API sends for a run, as this package declares it. */
export interface RunReply {
  id: string;
  state: string;
}

const RUNS_ORIGIN = process.env.RUNS_ORIGIN || "http://localhost:3000";

/** A request spec whose result is read for its `data` member, with a declared type. */
export async function readRunReply(options: RequestOptions): Promise<RunReply> {
  const reply = await (options.client ?? transport).get({ url: "/api/v1/runs/{id}", ...options });
  return reply.data as RunReply;
}

/** The same request, its result handed back whole. */
export function readRunResult(options: RequestOptions) {
  return (options.client ?? transport).get({ url: "/api/v1/runs/{id}", ...options });
}

/** A `fetch` whose result is read for a member that is not the body. */
export async function runExists(id: string): Promise<boolean> {
  const response = await fetch(`${RUNS_ORIGIN}/api/v1/runs/${id}`, { method: "GET" });
  return response.ok;
}
