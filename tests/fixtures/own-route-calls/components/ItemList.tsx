'use client';

import { useEffect, useState } from 'react';

type Item = { id: string; name: string; archived: boolean };

export function ItemList() {
  const [items, setItems] = useState<Item[]>([]);

  useEffect(() => {
    async function load() {
      const res = await fetch('/api/items');
      const data = await res.json();
      setItems(data.items);
    }
    load();
  }, []);

  async function addItem(name: string) {
    await fetch('/api/items', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ name }),
    });
  }

  return (
    <ul>
      {items.map((item) => (
        <li key={item.id} onClick={() => addItem(item.name)}>
          {item.name}
        </li>
      ))}
    </ul>
  );
}
