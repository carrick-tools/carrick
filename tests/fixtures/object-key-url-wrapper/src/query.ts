const BASE = process.env.THINGS_API_URL;

// A query builder: read where it is declared, and only there.
export function sharedQuery(cursor?: string): string {
  return cursor ? `?cursor=${cursor}` : "";
}

async function load({ url }: { url: string }) {
  const res = await fetch(url, { method: "GET" });
  return res.json();
}

export async function listReports(cursor?: string) {
  return await load({ url: `${BASE}/reports${sharedQuery(cursor)}` });
}
