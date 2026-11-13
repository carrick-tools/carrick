import { sendRequest, sendTimed, sendUpdate } from './net/transport.js';

function resolveOrigin(): string {
    return process.env['API_ORIGIN'] || 'https://api.example.test';
}

export function reportUsage(command: string): void {
    const body = { command };
    void sendRequest(new URL('/api/usage', resolveOrigin()), {
        method: 'POST',
        body: JSON.stringify(body)
    });
}

export async function ping(origin: string): Promise<unknown> {
    const res = await fetch(new URL('/api/ping', origin), { method: 'GET' });
    return res.json();
}

export async function heartbeat(origin: string): Promise<unknown> {
    const res = await sendTimed(new URL('/api/heartbeat', origin), { method: 'POST' });
    return res.json();
}

export async function saveSettings(origin: string): Promise<unknown> {
    const res = await sendUpdate(new URL('/api/settings', origin), { method: 'POST' });
    return res.json();
}
