import { Reply, AppReply, Bare } from 'fixture-reply';
import { Reply as ReExported } from './reexport';

interface User { id: string; name: string }
declare const user: User;
declare const found: boolean;
declare const hand: Reply;

// The repo's own class: its names match the library's, no label names its declarations.
class OwnReply {
  constructor(public body?: string | null) {}
  static json(body: unknown): OwnReply { return new OwnReply(JSON.stringify(body)); }
}

// A
export async function GET() {
  if (!found) return Reply.json({ error: 'nf' }, { status: 404 });
  return Reply.json(user);
}

// B
export async function POST() {
  if (!found) return new Reply(JSON.stringify({ error: 'nf' }), { status: 404 });
  return new Reply(JSON.stringify(user), { status: 200 });
}

// C
export async function PUT() {
  return new Reply(JSON.stringify(user));
}

// D
export async function PATCH() {
  if (!found) return Reply.rewrite('/login');
  return Reply.json(user);
}

// E
export async function DELETE() {
  if (!found) return new AppReply(JSON.stringify({ error: 'nf' }), { status: 404 });
  return new AppReply(JSON.stringify(user));
}

// F
export async function HEAD() {
  if (!found) return OwnReply.json({ error: 'nf' });
  return new OwnReply(JSON.stringify(user));
}

// G
export async function OPTIONS() {
  if (!found) return ReExported.json({ error: 'nf' }, { status: 404 });
  return ReExported.json(user);
}

// H
export async function TRACE() {
  if (!found) return new ReExported(JSON.stringify({ error: 'nf' }), { status: 404 });
  return new ReExported(JSON.stringify(user));
}

// I
export async function CONNECT() {
  if (!found) return new Bare();
  return Reply.json(user);
}

void hand;
