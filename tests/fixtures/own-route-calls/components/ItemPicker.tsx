'use client';

import { useEffect, useState } from 'react';
import { getItem, listItems, type Item } from '../lib/items-api';

export function ItemPicker({ selected }: { selected: string }) {
  const [items, setItems] = useState<Item[]>([]);

  useEffect(() => {
    listItems().then(setItems);
    getItem(selected).then((item) => setItems((all) => [...all, item]));
  }, [selected]);

  return (
    <select>
      {items.map((item) => (
        <option key={item.id}>{item.name}</option>
      ))}
    </select>
  );
}
