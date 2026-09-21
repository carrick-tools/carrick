import type { Parcel } from '../kit.ts';

// Colis — en attente (multi-byte text above the function this module
// publishes, reached only through the `@/` alias its tsconfig declares)
export async function listPending(): Promise<Parcel[]> {
  return [];
}
