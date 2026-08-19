import type { LedgerClient } from "ledger-client";

export function describe(client: LedgerClient): string {
  return client.describe();
}
