'use client';

type Item = { id: string; name: string };

export function ItemForm({ item }: { item?: Item }) {
  async function submit(values: { name: string }) {
    const url = item ? `/api/items/${item.id}` : '/api/items';
    const method = item ? 'PUT' : 'POST';
    await fetch(url, {
      method,
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(values),
    });
  }

  async function submitInline(values: { name: string }) {
    await fetch(item ? `/api/items/${item.id}` : '/api/items', {
      method: item ? 'PUT' : 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(values),
    });
  }

  return (
    <form onSubmit={() => submit({ name: 'x' })} onReset={() => submitInline({ name: 'y' })} />
  );
}
