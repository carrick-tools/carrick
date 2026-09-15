import { checkSlotAvailability } from '@/slots/mod.ts';

// ALIAS through a re-export barrel.
export function acceptReturn(lockerId: string): boolean {
  return checkSlotAvailability(lockerId);
}
