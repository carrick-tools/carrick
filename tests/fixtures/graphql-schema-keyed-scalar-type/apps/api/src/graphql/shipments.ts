import { kit } from './kit.ts';

interface ShipmentRecord {
  id: string;
  scheduledDate: Date;
}

function scheduleShipment(id: string): ShipmentRecord {
  return { id, scheduledDate: new Date() };
}

kit.mutationFields((t) => ({
  scheduleShipment: t.field({
    type: 'Shipment',
    args: { id: t.arg.id({ required: true }) },
    resolve: (_root, args): ShipmentRecord => scheduleShipment(String(args.id)),
  }),
}));
