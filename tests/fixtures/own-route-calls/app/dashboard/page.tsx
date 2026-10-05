type Item = { id: string; name: string; archived: boolean };

async function saveItem(item: Item | undefined, values: { name: string }) {
  'use server';
  const url = item ? `/api/items/${item.id}` : '/api/items';
  const method = item ? 'PUT' : 'POST';
  await fetch(url, { method, body: JSON.stringify(values) });
}

export default async function DashboardPage({
  searchParams,
}: {
  searchParams: Promise<{ id: string; from: string }>;
}) {
  const { id, from } = await searchParams;
  const itemsRes = await fetch('/api/items');
  const itemRes = await fetch(`/api/items/${id}`);
  const reportRes = await fetch(`/api/reports?from=${from}`);
  const item: Item = await itemRes.json();
  const method = item.archived ? 'DELETE' : 'POST';
  await fetch(`/api/items/${id}/archive`, { method });
  const settingsRes = await fetch(`${process.env.APP_URL}/api/settings`);

  return (
    <main>
      <pre>
        {JSON.stringify([await itemsRes.json(), item, await reportRes.json(), await settingsRes.json()])}
      </pre>
      <form action={saveItem.bind(null, item, { name: item.name })} />
    </main>
  );
}
