import * as client from './client';

export async function signIn(): Promise<string> {
  const session = await client.login('admin@example.com', 'secret');
  return session.token;
}

export async function signOut(): Promise<void> {
  await client.logout('everywhere');
}
