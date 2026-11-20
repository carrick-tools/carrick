import { useFn } from "@fixture/rpc";
import { saveNote } from "./notes.functions";
import { register } from "./value";

export async function both(): Promise<void> {
  const keep = useFn(saveNote);
  register(keep);
  await keep({ data: { body: "y" } });
}

function memo(run: () => unknown, deps: unknown[]): () => unknown {
  void deps;
  return run;
}

export function later(): () => unknown {
  const again = useFn(saveNote);
  return memo(() => again({ data: { body: "z" } }), [again]);
}
