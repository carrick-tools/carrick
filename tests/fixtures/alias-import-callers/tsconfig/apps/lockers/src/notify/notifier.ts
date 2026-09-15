import { LockerNotifier } from './notifier.service.ts';

// SINGLETON: one module-scope instance every importer shares. The class is
// declared one import away from the binding the callers import.
export const notifier = new LockerNotifier();
