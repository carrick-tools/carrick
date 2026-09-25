import type { LedgerFeed } from "./ledger-feed";

export class RemoteLedgerFeed implements LedgerFeed {
  publish(topic: string, body: unknown): void {
    void fetch(`/ledger/${topic}`, { method: "POST", body: JSON.stringify(body) });
  }
}
