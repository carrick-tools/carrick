import type { Reply } from './types';
import { callPlain } from './call';

async function retry<T>(run: () => Promise<T>): Promise<T> {
  return run();
}



export async function retried(url: string) {
  return retry(() => callPlain(url));
}

export async function envelope(url: string) {
  const res = await fetch(url);
  return { ok: true, data: (await res.json()) as Reply };
}
