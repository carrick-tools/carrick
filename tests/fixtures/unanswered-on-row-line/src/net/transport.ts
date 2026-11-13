export function asUpdate(init?: RequestInit): RequestInit {
    if (process.env['READ_ONLY']) {
        return init ?? {};
    }
    return { ...init, method: 'PUT' };
}

export function sendUpdate(input: string, init?: RequestInit): Promise<Response> {
    return fetch(input, asUpdate(init));
}
