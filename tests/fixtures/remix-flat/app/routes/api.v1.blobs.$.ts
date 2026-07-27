// A bare `$` segment is the splat: it matches everything that remains.

export const loader = makeStreamRoute(async () => new Response("blob"));

declare function makeStreamRoute(handler: () => Promise<Response>): unknown;
