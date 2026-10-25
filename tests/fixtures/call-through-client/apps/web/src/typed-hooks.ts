import { useQuery } from "@fixture/query";
import { readRunReply, readRunResult, readRunTyped, transport } from "@fixture/client";

/** No row from the model. A call straight into the client package, which parses a `fetch`. */
export async function loadRunTyped(id: string) {
  const reply = await readRunTyped(id);
  return reply;
}

/** The model states a row at the hook. The callback calls the same function. */
export function useRunTyped(id: string) {
  return useQuery({ queryKey: ["run", id], queryFn: () => readRunTyped(id) });
}

/** No row from the model. The function reads a member of a spec's result. */
export async function loadRunReply(id: string) {
  const reply = await readRunReply({ client: transport, path: { id } });
  return reply;
}

/** No row from the model. The function hands a spec's result back whole. */
export async function loadRunResult(id: string) {
  const result = await readRunResult({ client: transport, path: { id } });
  return result;
}
