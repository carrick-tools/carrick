export const limit = Number(process.env.LIMIT ?? "10");
export function toBuf(s: string): Buffer {
  return Buffer.from(s);
}
