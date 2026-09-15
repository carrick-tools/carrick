import { checkSlotAvailability } from '@/slots/availability.ts';
import { db } from '@/db.ts';

// ALIAS: same shapes as the control, imported through the import-map alias.
export class DeliveryService {
  plan(lockerId: string): boolean {
    return checkSlotAvailability(lockerId);
  }

  async confirm(lockerId: string): Promise<boolean> {
    return await db.transaction(async (tx) => {
      tx.write('deliveries', { lockerId });
      return checkSlotAvailability(lockerId);
    });
  }
}
