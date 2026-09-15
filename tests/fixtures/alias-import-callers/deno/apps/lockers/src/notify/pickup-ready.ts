import { notifier } from './notifier.ts';

// CONTROL: the singleton imported relatively.
export function announcePickupReady(lockerId: string): string {
  return notifier.notifyReady(lockerId);
}
