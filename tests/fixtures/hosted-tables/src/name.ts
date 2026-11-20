import { db } from "./client";

const T = "notes";

export async function readNotes() {
  return await db.from(T).select();
}
