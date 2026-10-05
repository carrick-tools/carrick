// A workspace package: its source is in this repository.

export interface Parser<T> {
  parse(value: unknown): T;
}

export async function sendJson<T>(
  parser: Parser<T>,
  url: string,
  init: { headers: Record<string, string> },
): Promise<T> {
  const response = await fetch(url, init);
  return parser.parse(await response.json());
}
