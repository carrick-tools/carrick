declare function express(): express.Express;

declare namespace express {
  interface Request {
    params: Record<string, string>;
    query: Record<string, unknown>;
    body: any;
  }

  interface Response {
    status(code: number): this;
    json(body?: unknown): this;
    send(body?: unknown): this;
  }

  type RequestHandler = (request: Request, response: Response) => unknown;

  interface IRouter {
    get(path: string, ...handlers: RequestHandler[]): this;
    post(path: string, ...handlers: RequestHandler[]): this;
    put(path: string, ...handlers: RequestHandler[]): this;
    delete(path: string, ...handlers: RequestHandler[]): this;
    use(...handlers: unknown[]): this;
  }

  interface Express extends IRouter {
    listen(port: number): unknown;
  }

  function json(): RequestHandler;
}

export default express;
export function Router(): express.IRouter;
