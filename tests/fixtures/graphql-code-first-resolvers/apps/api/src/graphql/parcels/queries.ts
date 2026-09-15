import { kit } from '../kit.ts';
import { findParcel, listParcels } from './store.ts';

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
}));
