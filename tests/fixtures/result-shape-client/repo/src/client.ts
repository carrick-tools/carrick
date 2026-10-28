import { api } from 'fixture-reply-client';

export interface User {
  id: string;
}

// 1. A reply wrapper: the body is the reply's `data`.
export async function loadUser(id: string) {
  const reply = await api.get<User>(`/users/${id}`);
  return reply.data;
}

// 2. The caller supplies the whole return.
export async function createUser(name: string) {
  return api.send<User>('/users', { name });
}

// 3. The store labels a member no signature offers: refused.
export async function health() {
  return api.ping('/health');
}

// 4. Two labelled calls on one line.
export async function both(id: string) {
  const [one, two] = [await api.get<User>(`/a/${id}`), await api.get<User>(`/b/${id}`)];
  return [one.data, two.data];
}
