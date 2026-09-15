import { checkSlotAvailability } from './mod.ts';

// CONTROL: relative import through a re-export barrel.
export function auditLocker(lockerId: string): string {
  return checkSlotAvailability(lockerId) ? 'open' : 'closed';
}
