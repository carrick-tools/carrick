// Negative: the client object is written through after it is made, so the
// function at its key is not known. A hole, never the factory's arrow.
import { createClient } from '../r1/client';

export const loose = createClient();

export function swap(next: typeof loose.get) {
  loose.get = next;
}

export const listLoose = () => loose.get({ url: '/n1/loose' });
