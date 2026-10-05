'use client';

export default function NewItemPage() {
  async function create(name: string) {
    await fetch('/api/items', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ name }),
    });
  }

  return <button onClick={() => create('Widget')}>Create</button>;
}
