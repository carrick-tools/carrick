import { client } from './instance';
import type { Client, RequestOptions } from './client/types';

export type Options<TData> = Omit<RequestOptions, 'url' | 'method'> & TData & { client?: Client };

export type ListWidgetsData = { url: '/api/v1/widgets'; query?: { page?: string } };
export type GetWidgetData = { url: '/api/v1/widgets/{id}'; path: { id: string } };
export type CreateWidgetData = { url: '/api/v1/widgets'; body: { name: string } };

export const listWidgets = (options?: Options<Partial<ListWidgetsData>>) => {
  return (options?.client ?? client).get({
    url: '/api/v1/widgets',
    ...options,
  });
};

export const getWidget = (options: Options<Omit<GetWidgetData, 'url'>>) => {
  return (options.client ?? client).get({
    url: '/api/v1/widgets/{id}',
    ...options,
  });
};

export const createWidget = (options: Options<Omit<CreateWidgetData, 'url'>>) => {
  return (options.client ?? client).post({
    url: '/api/v1/widgets',
    ...options,
    headers: { 'Content-Type': 'application/json', ...options.headers },
  });
};
