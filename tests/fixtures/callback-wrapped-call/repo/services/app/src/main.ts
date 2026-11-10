import { verify } from "@fixture/client";

declare function readId(): string;

const passed = new Map<string, boolean>();

export async function connect(): Promise<boolean> {
  const id = readId();
  // A guard written inside this function runs the check it is handed.
  async function guarded(key: string, check: () => Promise<boolean>): Promise<boolean> {
    if (passed.has(key)) return true;
    const ok = await check();
    passed.set(key, ok);
    return ok;
  }
  return guarded(
    id,
    () => verify(id),
  );
}
