export function formatMinor(amount: number, currency: string): string {
  const major = Math.trunc(amount / 100);
  const minor = String(Math.abs(amount % 100)).padStart(2, "0");
  return `${currency} ${major}.${minor}`;
}
