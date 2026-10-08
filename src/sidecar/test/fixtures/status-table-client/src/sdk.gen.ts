import { client, type Options, type RequestOptions } from './client/types';
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

// carrick#1841, request half. The body travels inside the spread options.
import type { CreateItemData, CreateItemResponses, ItemInput, RemoveItemData } from './types.gen';

export const createItem = <ThrowOnError extends boolean = false>(options: Options<CreateItemData, ThrowOnError>) =>
  client.post<CreateItemResponses, GetItemErrors, ThrowOnError>({
    url: '/items',
    ...options,
    headers: { 'Content-Type': 'application/json', ...options.headers },
  });

// The body written on the request object itself.
export const renameItem = (id: string, input: ItemInput) =>
  client.patch<GetItemResponses, GetItemErrors>({ url: '/items/{id}', path: { id }, body: input });

// An operation whose data type states no body.
export const removeItem = (options: Options<RemoveItemData>) =>
  client.delete<DeleteItemResponses, DeleteItemErrors>({ url: '/items/{id}', ...options });
