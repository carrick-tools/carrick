import type { Parcel } from '../kit.ts';

const parcels: Parcel[] = [];

export function listParcels(): Parcel[] {
  return parcels;
}

export function findParcel(id: string): Parcel | undefined {
  return parcels.find((parcel) => parcel.id === id);
}

export function dispatchParcel(id: string): Parcel {
  const parcel = { id, weightGrams: 0 };
  parcels.push(parcel);
  return parcel;
}

export function heaviestParcel(): Parcel | undefined {
  return [...parcels].sort((a, b) => b.weightGrams - a.weightGrams)[0];
}

// Colis — archives (multi-byte text above the sites this file publishes, so a
// span converted against the IMPORTING file's source would miss them)
export function listArchived(): Parcel[] {
  return [];
}

export const listRecentParcels = (): Parcel[] => parcels.slice(-5);
