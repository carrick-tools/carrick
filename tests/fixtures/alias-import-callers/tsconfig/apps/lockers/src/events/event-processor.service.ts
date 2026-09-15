import { queueCourierPickup } from '#pickup/queue.ts';

interface LockerEvent {
  kind: 'parcel.dropped' | 'parcel.collected';
  parcelId: string;
}

// SUBPATH IMPORT: the package.json `imports` map, not tsconfig, names this module.
export class EventProcessorService {
  async handle(event: LockerEvent): Promise<void> {
    switch (event.kind) {
      case 'parcel.dropped':
        await queueCourierPickup(event.parcelId);
        break;
      case 'parcel.collected':
        break;
    }
  }
}
