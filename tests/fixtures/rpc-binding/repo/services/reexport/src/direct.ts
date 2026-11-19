import { listNotes } from "./notes.functions";

export async function load(): Promise<unknown> {
  const rows = await listNotes();
  return rows;
}
