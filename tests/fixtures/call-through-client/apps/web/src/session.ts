import { recordAudit } from "@fixture/client";

export interface Session {
  run(): Promise<unknown>;
}

/** Records the opening, then hands back a session of its own. */
export async function openSession(): Promise<Session> {
  await recordAudit({ body: "open" });
  return { run: async () => undefined };
}
