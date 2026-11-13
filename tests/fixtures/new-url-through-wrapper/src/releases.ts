import { sendRequest } from './net/transport.js';

export async function confirm({ origin, body }: { origin: string; body: { version: string; notes: string } }): Promise<{ confirmed: boolean }> {
    const url = new URL('/api/releases/confirm', origin);
    const res = await sendRequest(url, {
        method: 'POST',
        body: JSON.stringify(body)
    });
    return (await res.json()) as { confirmed: boolean };
}

export async function release(origin: string, body: { version: string }): Promise<{ id: string }> {
    const url = new URL('/api/releases', origin);
    const res = await sendRequest(url.href, { method: 'POST', body: JSON.stringify(body) });
    return (await res.json()) as { id: string };
}

export async function preview(origin: string): Promise<unknown> {
    const url = new URL('/api/bundles/preview', origin);
    const res = await sendRequest(url.toString(), { method: 'POST' });
    return res.json();
}

export async function upload(origin: string, name: string): Promise<unknown> {
    const url = new URL(`/api/bundles/${name}/upload`, origin);
    const res = await sendRequest(`${url}`, { method: 'POST' });
    return res.json();
}
