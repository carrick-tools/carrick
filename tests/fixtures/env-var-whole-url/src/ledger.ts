// The four base shapes a persisted row has to tell apart (carrick#649). Every
// call here is a plain request the analyzer answers for; what differs is where
// each one's base comes from.
import type { LedgerEntry } from "./types";

// 1. INJECTED: the base is handed to this class as an option. The scanner sees
//    the expression and never the value.
export class LedgerClient {
  constructor(private readonly opts: { lookupUrl: string }) {}

  async lookup(id: string): Promise<LedgerEntry> {
    const res = await fetch(`${this.opts.lookupUrl}/api/lookup`, {
      method: "GET",
    });
    return res.json();
  }
}

// 2. RELATIVE: no base at all. The request goes wherever the client already
//    points.
export async function listEntries(): Promise<LedgerEntry[]> {
  const res = await fetch("/api/entries", { method: "GET" });
  return res.json();
}

// 3. ENV with a THIRD-PARTY default: the source states an origin, and it is not
//    this machine.
const quoteUrl = process.env.GATEWAY_URL ?? "https://api.example.com/v1/quote";

export async function quote(): Promise<string> {
  const res = await fetch(quoteUrl, { method: "POST" });
  return res.text();
}

// 4. ENV declared OPTIONAL with no default, in `config/env.ts`. Nothing in this
//    file says so — the declaration is a repo-wide fact.
const knowledgeUrl = process.env.KNOWLEDGE_URL;

export async function ask(question: string): Promise<string> {
  const res = await fetch(`${knowledgeUrl}/api/knowledge`, {
    method: "POST",
    body: JSON.stringify({ question }),
  });
  return res.text();
}
