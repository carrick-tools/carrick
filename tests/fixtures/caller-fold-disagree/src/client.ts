import { http } from '@fixture/http';
import { span } from '@fixture/trace';

export interface Item {
  id: string;
}

export function listItems(): Promise<Item[]> {
  return span('list-items', () => http.get<Item[]>('/api/items'));
}

export function replaceItem(id: string, item: Item): Promise<Item> {
  return span('replace-item', () => http.put<Item>(`/api/items/${id}`, item));
}

export function saveItem(item: Item): Promise<Item> {
  return span('save-item', () => http.post<Item>('/api/items', item));
}

export function fetchPath(path: string): Promise<Item[]> {
  return span('fetch-path', () => http.get<Item[]>(path));
}
