import { client, type RequestOptions } from './client/types';
import type {
  DeleteItemErrors,
  DeleteItemResponses,
  GetItemErrors,
  GetItemResponses,
  ItemCounts,
  ListItemsResponses,
} from './types.gen';

export const getItem = (o: { path: { id: string } }) =>
  client.get<GetItemResponses, GetItemErrors>({ url: '/items/{id}', ...o });

export const listItems = <ThrowOnError extends boolean = false>(
  o?: Omit<RequestOptions<ThrowOnError>, 'url'>
) => client.get<ListItemsResponses, unknown, ThrowOnError>({ url: '/items', ...o });

export const deleteItem = (o: { path: { id: string } }) =>
  client.delete<DeleteItemResponses, DeleteItemErrors>({ url: '/items/{id}', ...o });

declare function fetchCounts(url: string): Promise<ItemCounts>;

export async function loadCounts(): Promise<ItemCounts> {
  const counts = await fetchCounts('/items/counts');
  return counts;
}
