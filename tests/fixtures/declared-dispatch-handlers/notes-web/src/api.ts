export interface CreditRequest {
  op: "credit";
  account: string;
  amount: number;
}

export interface CreditReply {
  account: string;
  balance: number;
}

export interface AddRequest {
  kind: "add";
  text: string;
  tags: string[];
}

// Wrong on purpose: the service answers `{ note: { id, text } }`.
export interface AddReply {
  note: { id: string; body: string };
}

export interface ListRequest {
  kind: "list";
  limit: number;
}

export interface ListReply {
  notes: { id: string; text: string }[];
}

export interface PurgeRequest {
  kind: "purge";
}

export interface PurgeReply {
  purged: number;
}

export interface ArchiveRequest {
  kind: "archive";
}

export interface ArchiveReply {
  archived: boolean;
}

export async function creditAccount(account: string, amount: number): Promise<CreditReply> {
  const input: CreditRequest = { op: "credit", account, amount };
  const response = await fetch("/ledger", { method: "POST", body: JSON.stringify(input) });
  const reply: CreditReply = await response.json();
  return reply;
}

export async function addNote(text: string, tags: string[]): Promise<AddReply> {
  const input: AddRequest = { kind: "add", text, tags };
  const response = await fetch("/notes", { method: "POST", body: JSON.stringify(input) });
  const reply: AddReply = await response.json();
  return reply;
}

export async function listNotes(limit: number): Promise<ListReply> {
  const input: ListRequest = { kind: "list", limit };
  const response = await fetch("/notes", { method: "POST", body: JSON.stringify(input) });
  const reply: ListReply = await response.json();
  return reply;
}

export async function purgeNotes(): Promise<PurgeReply> {
  const input: PurgeRequest = { kind: "purge" };
  const response = await fetch("/notes", { method: "POST", body: JSON.stringify(input) });
  const reply: PurgeReply = await response.json();
  return reply;
}

export async function archiveNotes(): Promise<ArchiveReply> {
  const input: ArchiveRequest = { kind: "archive" };
  const response = await fetch("/notes", { method: "POST", body: JSON.stringify(input) });
  const reply: ArchiveReply = await response.json();
  return reply;
}

export async function replay(command: Record<string, unknown>): Promise<unknown> {
  const response = await fetch("/notes", { method: "POST", body: JSON.stringify(command) });
  return response.json();
}
