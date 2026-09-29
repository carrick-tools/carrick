// src/pages/api/notes.ts serves /api/notes. The routes are stated by the
// file's location and its exported method names; nothing here is a call that
// registers a route.
import type { Note, NoteList } from "../../lib/types";

const notes: Note[] = [{ id: "1", title: "First", body: "Hello" }];

export async function GET(): Promise<Response> {
  const list: NoteList = { notes, total: notes.length };
  return Response.json(list);
}

export async function POST({ request }: { request: Request }): Promise<Response> {
  const form = await request.formData();
  const title = form.get("title");
  if (typeof title !== "string" || title === "") {
    return new Response("Missing title.", { status: 400 });
  }
  const created: Note = { id: String(notes.length + 1), title, body: "" };
  notes.push(created);
  return Response.json(created, { status: 201 });
}
