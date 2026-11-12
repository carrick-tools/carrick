// Every declaration below sits in a file that imports Item, so a mention of
// the name is kept in the graph whenever the statement holds the identifier.
import type { Item } from './item.js';
import { makeItem } from './make.js';

// A written use: the parameter is typed Item.
export function written(value: Item): string {
  return value.id;
}

// An object key spelled Item: an identifier, so the graph keeps it.
export const keyOnly = { Item: 1 };

// A local that shadows Item: an identifier, so the graph keeps it.
export function shadowed(): number {
  const Item = 2;
  return Item;
}

// The string 'Item': no identifier, so the graph has no mention.
export const stringOnly = 'Item';

// Returns an Item without writing the name: the graph has no mention, the
// compiler's inferred return type is the only record of it.
export function inferredOnly() {
  return makeItem();
}
