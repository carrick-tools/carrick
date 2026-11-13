import { sendUpdate } from './net/transport.js';

export async function saveSettings(): Promise<unknown> {
    const res = await sendUpdate('/api/settings', { method: 'POST' });
    return res.json();
}

export async function ping(): Promise<unknown> {
    const res = await fetch('/api/ping', { method: 'GET' });
    return res.json();
}
