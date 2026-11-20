import { useFn } from "@fixture/rpc";
import { saveNote } from "./notes.functions";
import { register } from "./value";

export async function both(): Promise<void> {
  const keep = useFn(saveNote);
  register(keep);
  await keep({ data: { body: "y" } });
}
