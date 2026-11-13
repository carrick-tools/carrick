import type { Ticket } from '../types';

export async function readTicket(path: string): Promise<Ticket> {
  const res = await fetch(`/v1${path}`);
  return res.json();
}
