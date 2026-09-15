import { queueCourierPickup } from '@/pickup/queue.ts';

interface LockerEvent {
  kind: 'parcel.dropped' | 'parcel.collected';
  parcelId: string;
}

// ALIAS: the only caller of queueCourierPickup, a direct call in a class method.
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
