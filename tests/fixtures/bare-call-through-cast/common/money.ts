export function formatTotal(c: number): string {
  return String(c);
}

export function roundCents(n: number): number {
  return Math.round(n * 100) / 100;
}
