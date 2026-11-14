import type { Parcel } from '../models';

function findParcel(id: string): Parcel | null {
  return id === '' ? null : { id, weightGrams: 1 };
}

export const fields = {
  parcel: {
    resolve: (_root: unknown, args: { id: string }) => findParcel(args.id),
  },
};
