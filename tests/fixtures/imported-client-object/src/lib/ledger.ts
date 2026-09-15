async function ledgerRequest(path: string) {
  const res = await fetch(`/ledger${path}`, { method: "GET" });
  return res.json();
}

function entriesPath(kind: string): string {
  switch (kind) {
    case "open":
      return "/entries/open";
    default:
      return "/entries";
  }
}

export async function loadEntries(kind: string) {
  return ledgerRequest(entriesPath(kind));
}

export async function loadTotals() {
  return ledgerRequest("/totals");
}
