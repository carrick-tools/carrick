let shared: { keepalive: boolean } | undefined;

function sharedAgent(): { keepalive: boolean } | undefined {
    if (!process.env['HTTP_AGENT']) {
        return undefined;
    }
    shared ??= { keepalive: true };
    return shared;
}

export function withAgent(init?: RequestInit): RequestInit {
    const agent = sharedAgent();
    if (!agent) {
        return init ?? {};
    }
    // The agent's type drifts from the platform's; same object at runtime.
    return { ...init, dispatcher: agent } as unknown as RequestInit;
}

export function withTimeout(init?: RequestInit): RequestInit {
    return { ...init, signal: AbortSignal.timeout(5000) };
}

export function asUpdate(init?: RequestInit): RequestInit {
    if (process.env['READ_ONLY']) {
        return init ?? {};
    }
    return { ...init, method: 'PUT' };
}

export function sendRequest(input: string | URL | Request, init?: RequestInit): Promise<Response> {
    return fetch(input, withAgent(init));
}

export function sendTimed(input: string | URL | Request, init?: RequestInit): Promise<Response> {
    return fetch(input, withTimeout(init));
}

export function sendUpdate(input: string | URL | Request, init?: RequestInit): Promise<Response> {
    return fetch(input, asUpdate(init));
}
