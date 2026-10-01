const REPORTS = "/api/reports";

export async function ownFetch(seen: Set<string>) {
  if (seen.size > 0) {
    const fetch = async (key: string) => seen.has(key);
    return fetch(REPORTS);
  }
  return null;
}

export async function platformFetch() {
  return fetch(REPORTS);
}
