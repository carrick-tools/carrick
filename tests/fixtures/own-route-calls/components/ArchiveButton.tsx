'use client';

export function ArchiveButton({ id, archived }: { id: string; archived: boolean }) {
  async function toggle() {
    const method = archived ? 'DELETE' : 'POST';
    await fetch(`/api/items/${id}/archive`, { method });
  }

  async function saveSettings(method: 'PUT' | 'GET', currency?: string) {
    await fetch('/api/settings', {
      method: method,
      body: currency ? JSON.stringify({ currency }) : undefined,
    });
  }

  return (
    <button onClick={toggle} onDoubleClick={() => saveSettings('PUT', 'EUR')}>
      {archived ? 'Restore' : 'Archive'}
    </button>
  );
}
