'use client';

import { useState } from 'react';

export function ReportTable({ itemId }: { itemId: string }) {
  const [rows, setRows] = useState<unknown[]>([]);

  async function byRange(from: string, to: string) {
    const res = await fetch(`/api/reports?from=${from}&to=${to}`);
    setRows((await res.json()).rows);
  }

  async function byParams(from: string, to: string) {
    const params = new URLSearchParams({ from, to });
    const res = await fetch(`/api/reports?${params.toString()}`);
    setRows((await res.json()).rows);
  }

  async function history(page: number) {
    const base = `/api/items/${itemId}/history`;
    const res = await fetch(`${base}?page=${page}`);
    setRows((await res.json()).events);
  }

  async function byConcat(from: string, to: string) {
    const params = new URLSearchParams({ from, to });
    const res = await fetch('/api/reports?' + params.toString());
    setRows((await res.json()).rows);
  }

  async function bySuffix(from?: string) {
    const query = from ? `?from=${from}` : '';
    const res = await fetch(`/api/reports${query}`);
    setRows((await res.json()).rows);
  }

  return (
    <table onClick={() => byRange('a', 'b')} onDoubleClick={() => byParams('a', 'b')}>
      <tbody onFocus={() => history(1)} onBlur={() => byConcat('a', 'b')}>
        <tr>
          <td onClick={() => bySuffix('a')}>{rows.length}</td>
        </tr>
      </tbody>
    </table>
  );
}
