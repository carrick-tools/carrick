export function handler(payload: { ok: boolean; count: number }): { statusCode: number; body: string } {
  return { statusCode: 200, body: JSON.stringify(payload) };
}
