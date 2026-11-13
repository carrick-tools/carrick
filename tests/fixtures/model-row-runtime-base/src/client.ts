function apiBase(): string {
    return process.env['SERVICE_ORIGIN'] || 'https://service.example.test';
}

export async function listThings(): Promise<unknown> {
    const res = await fetch(new URL('things/list', apiBase()));
    return res.json();
}

export async function listThingsTemplate(): Promise<unknown> {
    const res = await fetch(`${apiBase()}/things/template`);
    return res.json();
}

export async function ping(): Promise<unknown> {
    const res = await fetch('/health');
    return res.json();
}
