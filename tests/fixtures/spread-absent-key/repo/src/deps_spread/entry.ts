// Two callers of the action: one hands it the factory's members as they are,
// the other spreads them and adds a member of its own.
import { tableDeps } from './table';
import { runAction, runLoader, runParsedAction } from './action';

export function plain(key: string): Promise<unknown> {
  return runAction(key, tableDeps());
}

export function minted(key: string): Promise<unknown> {
  return runAction(key, { ...tableDeps(), mint: () => 'token' });
}

export function parsed(key: string, raw: string): Promise<unknown> {
  return runParsedAction(key, JSON.parse(raw));
}

export function loaded(key: string, raw: string): Promise<unknown> {
  return runLoader(key, JSON.parse(raw));
}
