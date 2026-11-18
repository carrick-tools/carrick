// Deployed as a function, but hands no handler to a top-level call: the
// route exists (the folder says so), and nothing here says what serves it.
export async function buildReport(rows: string[]): Promise<string> {
  return rows.join("\n");
}
