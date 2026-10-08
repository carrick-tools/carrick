import { removeItem } from './handlers';

export interface Item {
  id: string;
  name: string;
}

export function getItem(): Item {
  return { id: '1', name: 'first' };
}

export const listItems = (): Item[] => [{ id: '1', name: 'first' }];

const itemController = {
  count(): { total: number } {
    return { total: 1 };
  },
};

export const routes = [
  { method: 'GET', path: '/items/:id', handler: getItem },
  { method: 'GET', path: '/items', handler: listItems },
  { method: 'DELETE', path: '/items/:id', handler: removeItem },
  { method: 'GET', path: '/items/first', handler: (): Item => ({ id: '1', name: 'first' }) },
  { method: 'GET', path: '/items/count', handler: itemController.count },
];
