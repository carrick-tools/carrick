// A default import, a namespace import and an import through the barrel.
import type DefaultItem from './default-item.js';
import * as items from './item.js';
import type { Item as BarrelItem } from './barrel.js';

export function fromDefault(value: DefaultItem): string {
  return value.id;
}

export function fromNamespace(value: items.Item): string {
  return value.id;
}

export function fromBarrel(value: BarrelItem): string {
  return value.id;
}
