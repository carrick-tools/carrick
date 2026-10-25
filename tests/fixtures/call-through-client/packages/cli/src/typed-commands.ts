import { readRunReply, readRunResult, runExists, transport } from "@fixture/client";
import { createTypedClient, type RunState, type RunView } from "./typed-client";
import type { Run, SavedRun } from "./types";

/** The model states a row here. The client reads a member of a spec's result. */
export async function saveTyped(id: string) {
  const client = createTypedClient();
  const saved: SavedRun = await client.saveRun(id, "done");
  return saved;
}

/** The model states a row here. The client parses a `fetch`. */
export async function readTyped(id: string) {
  const client = createTypedClient();
  const run: Run = await client.read(id);
  return run;
}

/** The model states a row here. The client maps the value before it returns it. */
export async function viewTyped(id: string) {
  const client = createTypedClient();
  const view: RunView = await client.getRunView(id);
  return view;
}

/** The model states a row here. The client reads a member of the run. */
export async function stateTyped(id: string) {
  const client = createTypedClient();
  const state: RunState = await client.getRunState(id);
  return state;
}

/** No row from the model. */
export async function getTyped(id: string) {
  const client = createTypedClient();
  const run: Run = await client.getRun(id);
  return run;
}

/** No row from the model. */
export async function replyTyped(id: string) {
  const reply = await readRunReply({ client: transport, path: { id } });
  return reply;
}

/** No row from the model. */
export async function resultTyped(id: string) {
  const result = await readRunResult({ client: transport, path: { id } });
  return result;
}

/** No row from the model. */
export async function existsTyped(id: string) {
  const exists = await runExists(id);
  return exists;
}
