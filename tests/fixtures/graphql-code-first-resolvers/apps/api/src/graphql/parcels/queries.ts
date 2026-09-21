import { kit } from '../kit.ts';
import * as store from './store.ts';
import { findParcel, heaviestParcel, listParcels, listRecentParcels } from './store.ts';

// Colis — lecture
kit.queryFields((t) => ({
  parcels: t.field({
    type: ['Parcel'],
    resolve: () => listParcels(),
  }),
  parcel: t.field({
    type: 'Parcel',
    nullable: true,
    args: { id: t.arg.id({ required: true }) },
    resolve: (_root, args) => findParcel(String(args.id)),
  }),
  heaviestWeight: t.int({
    nullable: true,
    validate: (_args: unknown) => true,
    resolve: () => heaviestParcel()?.weightGrams,
  }),
  recentParcels: t.field({
    type: ['Parcel'],
    resolve: listRecentParcels,
  }),
  parcelCount: t.int({
    resolve: countParcels,
  }),
  archivedParcels: t.field({
    type: ['Parcel'],
    resolve: store.listArchived,
  }),
}));

function countParcels(): number {
  return listParcels().length;
}
