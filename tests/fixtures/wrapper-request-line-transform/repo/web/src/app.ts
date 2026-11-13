import { getJson } from './net/envelope';
import { exchangeToken } from './net/token';
import { readTicket } from './net/read';
import { streamPost } from './net/stream';
import { send } from './net/send';
import { createApiClient } from './net/api-client';

export const listTickets = () => getJson('/tickets');
export const login = () => exchangeToken('/tokens');
export const latest = () => readTicket('/tickets/latest');
export const addNote = () => streamPost('/notes', { text: 'hi' });
export const postNote = () => send('/v1/notes', { method: 'POST', body: JSON.stringify({ text: 'hi' }) });

export async function askNote(text: string): Promise<void> {
  const client = createApiClient();
  const res = await client.streamPost('/notes', {
    text,
  });
  await res.body?.cancel();
}
