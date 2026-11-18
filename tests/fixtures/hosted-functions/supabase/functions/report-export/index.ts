// Sits where a function would, but hands no handler to a top-level call:
// nothing here says what serves the route.
export async function buildReport(rows: string[]): Promise<string> {
  return rows.join("\n");
}
