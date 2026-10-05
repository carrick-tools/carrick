'use client';

import { useEffect, useState } from 'react';

export function ItemDetail({ id }: { id: string }) {
  const [name, setName] = useState('');

  useEffect(() => {
    async function load() {
      const res = await fetch(`/api/items/${id}`);
      const item = await res.json();
      setName(item.name);
    }
    load();
  }, [id]);

  async function remove() {
    await fetch(`/api/items/${id}`, { method: 'DELETE' });
  }

  return <button onClick={remove}>{name}</button>;
}
