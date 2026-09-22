// src/pages/api/notes/[id].ts serves /api/notes/:id. The handler reads its
// body and calls out to another service; both are calls the handler MAKES,
// and neither is where the route is registered.
import type { Note } from "../../../lib/types";

export async function PUT({
  params,
  request,
}: {
  params: { id?: string };
  request: Request;
}): Promise<Response> {
  const patch = (await request.json()) as { title: string; body: string };
  await fetch("https://audit.internal/events", {
    method: "POST",
    body: JSON.stringify({ note: params.id, action: "update" }),
  });
  const updated: Note = { id: params.id ?? "", title: patch.title, body: patch.body };
  return Response.json(updated);
}
