/**
 * carrick#2743: a consumer call returned whole from a function that declares
 * it returns no body (`Promise<void>`, `Promise<undefined>`, or a response
 * object of `void`) states no body to judge.
 *
 * The clients below are hand-written and synthetic. Both verbs leave the
 * result's type parameter `R` free: only the result mentions it, so in
 * source the compiler takes it from the return position (`void`), and the
 * file compiles. The retype writes the producer's body as the call's first
 * type argument; the rest then take their defaults instead of being
 * inferred, and the declared return reports the default response object.
 * That diagnostic is about the written argument, not a body anyone reads.
 *
 * `plain` defaults `R` to the response object; `cond` reads it through a
 * conditional alias on a unique-symbol default, as newer clients do.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';
const CLIENT_DECL = `declare module 'plain-client' {
  export interface ClientResponse<T = any, D = any> {
    data: T;
    status: number;
    sent?: D;
  }
  export interface ClientInstance {
    post<T = any, R = ClientResponse<T>, D = any>(url: string, data?: D): Promise<R>;
  }
  export function create(): ClientInstance;
}

declare module 'cond-client' {
  const DEFAULT: unique symbol;
  export type Default = typeof DEFAULT;
  export interface CondResponse<T = any> {
    data: T;
    status: number;
  }
  export type Result<T, R> = R extends Default ? CondResponse<T> : R;
  export interface CondInstance {
    post<T = any, R = Default, D = any>(url: string, data?: D): Promise<Result<T, R>>;
  }
  export function create(): CondInstance;
}
`;
const CONSUMER = `import { create, type ClientResponse } from 'plain-client';
import { create as createCond, type CondResponse } from 'cond-client';

const api = create();
const cond = createCond();

export interface Job {
  x: number;
}

declare function wrap<T>(value: T): T;

export async function startJob(): Promise<void> {
  return api.post('/start');
}

export async function startAwaited(): Promise<void> {
  return await api.post('/start-awaited');
}

export async function startUndefined(): Promise<undefined> {
  return api.post('/start-undefined');
}

export async function removeJob(): Promise<ClientResponse<void>> {
  return await api.post('/remove');
}

export const startArrow = async (): Promise<void> => api.post('/start-arrow');

export async function startCond(): Promise<void> {
  return cond.post('/start-cond');
}

export async function removeCond(): Promise<CondResponse<void>> {
  return await cond.post('/remove-cond');
}

export async function fetchNoBody(): Promise<void> {
  const res = await fetch('/fetch-no-body');
  return res.json();
}

export async function wrapped(): Promise<void> {
  return wrap((await api.post('/wrapped')).data.x) ? undefined : undefined;
}

export async function getJob(): Promise<ClientResponse<Job>> {
  return api.post('/job');
}

export async function getCondJob(): Promise<CondResponse<Job>> {
  return await cond.post('/cond-job');
}
`;
/** The 1-based line of the one CONSUMER line holding `marker`. */
const lineOf = (marker) => {
    const lines = CONSUMER.split('\n');
    const at = lines.findIndex((line) => line.includes(marker));
    assert.ok(at >= 0 && lines.findIndex((l, i) => i > at && l.includes(marker)) < 0, marker);
    return at + 1;
};
const site = (receiver, url) => ({
    line: lineOf(`${receiver}.post('${url}')`),
    text: `${receiver}.post('${url}')`,
});
const NO_BODY = {
    startJob: site('api', '/start'),
    startAwaited: site('api', '/start-awaited'),
    startUndefined: site('api', '/start-undefined'),
    removeJob: site('api', '/remove'),
    startArrow: site('api', '/start-arrow'),
    startCond: site('cond', '/start-cond'),
    removeCond: site('cond', '/remove-cond'),
    fetchNoBody: { line: lineOf("fetch('/fetch-no-body')"), text: "fetch('/fetch-no-body')" },
};
const JUDGED = {
    wrapped: site('api', '/wrapped'),
    getJob: site('api', '/job'),
    getCondJob: site('cond', '/cond-job'),
};
describe('carrick#2743: a call returned from a function that declares no body', () => {
    let client;
    let repoDir;
    let consumerPath;
    before(async () => {
        repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-2743-'));
        fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
        fs.writeFileSync(path.join(repoDir, 'tsconfig.json'), JSON.stringify({
            compilerOptions: {
                strict: true,
                module: 'esnext',
                moduleResolution: 'bundler',
                target: 'es2022',
                lib: ['es2022', 'dom'],
                skipLibCheck: true,
            },
            include: ['src'],
        }));
        fs.writeFileSync(path.join(repoDir, 'src', 'clients.d.ts'), CLIENT_DECL);
        consumerPath = path.join(repoDir, 'src', 'jobs.ts');
        fs.writeFileSync(consumerPath, CONSUMER);
        client = new SidecarClient();
        await client.start();
        await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
    });
    after(async () => {
        await client.stop();
        fs.rmSync(repoDir, { recursive: true, force: true });
    });
    async function retype(name, at, producerType) {
        const res = await client.send({
            action: 'retype_check',
            request_id: `${name}-${producerType}`,
            items: [
                {
                    item_id: name,
                    file_path: consumerPath,
                    line_number: at.line,
                    expression_text: at.text,
                    expression_line: at.line,
                    producer_type: producerType,
                    wire: true,
                },
            ],
        });
        assert.strictEqual(res.status, 'success', JSON.stringify(res));
        assert.strictEqual(res.outcomes?.length, 1);
        return res.outcomes[0];
    }
    it('the fixture compiles as written', async () => {
        const res = await client.send({
            action: 'retype_check',
            request_id: 'baseline',
            items: [
                {
                    item_id: 'baseline',
                    file_path: consumerPath,
                    line_number: JUDGED.getJob.line,
                    expression_text: JUDGED.getJob.text,
                    expression_line: JUDGED.getJob.line,
                    producer_type: '{ x: number; }',
                    wire: true,
                },
            ],
        });
        // A clean agreement on the matching body means the file raised nothing
        // the retype added; a broken fixture would surface as a diagnostic.
        assert.strictEqual(res.outcomes?.[0]?.outcome, 'agrees', JSON.stringify(res));
    });
    for (const [name, at] of Object.entries(NO_BODY)) {
        it(`(${name}) abstains: the function declares it returns no body`, async () => {
            const out = await retype(name, at, '{ y: number; }');
            assert.strictEqual(out.outcome, 'abstain', JSON.stringify(out));
            assert.match(out.reason ?? '', /declares it returns no body/);
        });
    }
    it('(wrapped) still judges a body read inside a function that returns nothing', async () => {
        const out = await retype('wrapped', JUDGED.wrapped, '{ y: number; }');
        assert.strictEqual(out.outcome, 'mismatch', JSON.stringify(out));
        assert.strictEqual(out.diagnostics[0].code, 2339);
    });
    for (const name of ['getJob', 'getCondJob']) {
        it(`(${name}) still judges a declared response object of a body`, async () => {
            const mismatch = await retype(name, JUDGED[name], '{ y: number; }');
            assert.strictEqual(mismatch.outcome, 'mismatch', JSON.stringify(mismatch));
            assert.strictEqual(mismatch.diagnostics[0].line, JUDGED[name].line);
            const agrees = await retype(name, JUDGED[name], '{ x: number; }');
            assert.strictEqual(agrees.outcome, 'agrees', JSON.stringify(agrees));
        });
    }
});
