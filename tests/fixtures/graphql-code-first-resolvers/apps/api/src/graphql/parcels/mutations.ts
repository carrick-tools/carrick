import { kit } from '@/graphql/kit';
import { dispatchParcel } from './store.ts';

kit.mutationFields((t) => ({
  dispatchParcel: t.field({
    type: 'Parcel',
    args: { id: t.arg.id({ required: true }) },
    resolve: (_root, args) => dispatchParcel(String(args.id)),
  }),
}));
