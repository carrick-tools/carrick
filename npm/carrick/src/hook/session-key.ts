// The key every hook files its per-session record under (carrick#3965).
//
// A sub-agent's tool calls reach the hooks with the parent's `session_id` and
// an `agent_id` of their own (the main agent's calls carry no `agent_id`), and
// the main agent's Stop is the only Stop. Keyed on `session_id` alone, a
// sub-agent's edits were recorded as the parent's and reported to the parent at
// its Stop. The key adds the `agent_id`, so the sub-agent's edits, reads, "said"
// marks and queued re-checks live in a record of their own.
//
// A leaf module: the hooks that stay light on purpose import it.

/** A file name a session id can never escape from. */
export function safeId(sessionId: string): string | null {
  return /^[A-Za-z0-9._-]{1,128}$/.test(sessionId) ? sessionId : null;
}

/**
 * The record key for one hook payload: the `session_id` for the main agent,
 * `<session_id>.<agent_id>` for a sub-agent, undefined with no session id. An
 * `agent_id` with characters a file name cannot carry has them replaced, so two
 * sub-agents still never share the parent's record.
 */
export function sessionKey(payload: { session_id?: string; agent_id?: string }): string | undefined {
  const session = payload.session_id;
  if (!session) return undefined;
  const agent = payload.agent_id;
  if (typeof agent !== "string" || agent === "") return session;
  return `${session}.${agent.replace(/[^A-Za-z0-9._-]/g, "_")}`;
}
