import { useFn } from "@fixture/rpc";
import { saveNote } from "./notes.functions";

export async function onSave(): Promise<void> {
  const save = useFn(saveNote);
  await save({ data: { body: "x" } });
}
