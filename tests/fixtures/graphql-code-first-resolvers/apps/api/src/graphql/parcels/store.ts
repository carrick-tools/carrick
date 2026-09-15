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
