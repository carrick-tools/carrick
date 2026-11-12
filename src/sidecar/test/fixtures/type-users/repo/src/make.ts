// A function whose written return type is Item.
import type { Item } from './item.js';

export function makeItem(): Item {
  return { id: 'made' };
}
