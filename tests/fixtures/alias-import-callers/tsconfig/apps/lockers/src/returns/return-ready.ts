import { notifier } from '@/notify/notifier.ts';

// ALIAS: the same singleton imported through the alias.
export function announceReturnReady(lockerId: string): string {
  return notifier.notifyReady(lockerId);
}
