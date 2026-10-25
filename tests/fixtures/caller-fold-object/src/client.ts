import { http } from '@fixture/http';
import { span } from '@fixture/trace';

export interface Item {
  id: string;
}

export const itemsApi = {
  replace: async (id: string, item: Item): Promise<Item> => {
    return span('replace-item', () => http.put<Item>(`/api/items/${id}`, item));
  },
  async save(item: Item): Promise<Item> {
    return span('save-item', () => http.post<Item>('/api/items', item));
  },
};
