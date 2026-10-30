import { login, logout } from './client';

export async function signIn(): Promise<string> {
  const session = await login('admin@example.com', 'secret');
  return session.token;
}

export async function signOut(): Promise<void> {
  await logout('everywhere');
}
