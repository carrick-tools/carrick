// A base URL read from the environment, interpolated into a template literal
// whose PATH carries a call expression in one of its segments. The call is an
// ordinary encoder around a value: structurally it is a path parameter, the
// same as `${region}` below, and the parentheses are inside the placeholder
// rather than left over in the route.
const INGEST_BASE = process.env.INGEST_URL || "https://ingest.example.invalid";
const DATASET = process.env.INGEST_DATASET ?? "default";

export async function forwardEvents(events: unknown[]): Promise<string> {
  const res = await fetch(
    `${INGEST_BASE}/v1/ingest/${encodeURIComponent(DATASET)}`,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(events),
    },
  );
  return res.text();
}

// The control: the same base, the same shape, an identifier in the segment
// instead of a call. This one has always been recorded.
export async function readStatus(region: string): Promise<string> {
  const res = await fetch(`${INGEST_BASE}/v1/status/${region}`, {
    method: "GET",
  });
  return res.text();
}
