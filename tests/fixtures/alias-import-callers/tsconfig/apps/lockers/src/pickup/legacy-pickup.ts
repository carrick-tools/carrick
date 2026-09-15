import { queueCourierPickup } from '~/pickup/queue.ts';

// BUNDLER-ONLY ALIAS: `~` is declared in vite.config.ts and nowhere else.
export async function legacyPickup(parcelId: string): Promise<string> {
  return await queueCourierPickup(parcelId);
}
