import createClient from 'fixture-fetcher';

export async function fetchChained() {
  return createClient('http://kit.internal').get('/kit/chained');
}
export async function fetchHeld() {
  const client = createClient('http://kit.internal');
  return client.get('/kit/held');
}
