import { checkSlotAvailability } from './availability.ts';
import { db } from '../db.ts';

// CONTROL: relative import, class methods, one call inside a transaction callback.
export class ReservationService {
  reserve(lockerId: string): boolean {
    return checkSlotAvailability(lockerId);
  }

  async reserveInTransaction(lockerId: string): Promise<boolean> {
    return await db.transaction(async (tx) => {
      tx.write('reservations', { lockerId });
      return checkSlotAvailability(lockerId);
    });
  }
}
