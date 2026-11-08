export interface Envelope {
  statusCode: number;
  headers?: Record<string, string>;
  body: string;
}

export interface Note {
  id: string;
  text: string;
}

export interface AddNoteInput {
  text: string;
  tags: string[];
}

export interface AddNoteResult {
  note: Note;
}

export interface ListNotesInput {
  query?: string;
  limit: number;
}

export interface NotesInput {
  kind: string;
  [field: string]: unknown;
}

interface Ctx {
  requestId: string;
}

interface LambdaEvent {
  body: string | null;
  requestContext: { requestId: string };
}

function reply<T>(statusCode: number, payload: T): Envelope {
  return {
    statusCode,
    headers: { "content-type": "application/json" },
    body: JSON.stringify(payload),
  };
}

export async function addNote(input: AddNoteInput, ctx: Ctx): Promise<Envelope> {
  if (!input.text) {
    return reply(400, { error: "text is required" });
  }
  return reply<AddNoteResult>(201, { note: { id: ctx.requestId, text: input.text } });
}

export async function listNotes(input: ListNotesInput, ctx: Ctx): Promise<Envelope> {
  console.log(ctx.requestId, input.query);
  const notes: Note[] = [];
  return { statusCode: 200, body: JSON.stringify({ notes: notes.slice(0, input.limit) }) };
}

export async function purgeNotes(input: NotesInput, ctx: Ctx): Promise<Envelope> {
  console.log(ctx.requestId, input.kind);
  const purged = 0;
  return reply(200, { purged });
}

export async function archiveNotes(input: NotesInput, ctx: Ctx): Promise<Envelope> {
  console.log(ctx.requestId, input.kind);
  const out = reply(200, { archived: true });
  return out;
}

export async function handler(event: LambdaEvent): Promise<Envelope> {
  const { kind, ...input } = JSON.parse(event.body ?? "{}") as NotesInput;
  const ctx: Ctx = { requestId: event.requestContext.requestId };
  switch (kind) {
    case "add":
      return addNote(input as unknown as AddNoteInput, ctx);
    case "list":
    case "search":
      return listNotes(input as unknown as ListNotesInput, ctx);
    case "purge":
      return purgeNotes({ kind, ...input }, ctx);
    case "archive":
      return archiveNotes({ kind, ...input }, ctx);
    default:
      return reply(400, { error: `unknown kind ${kind}` });
  }
}
