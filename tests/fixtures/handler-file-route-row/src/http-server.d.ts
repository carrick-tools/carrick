declare module "http-server" {
  export interface Request {
    params: Record<string, string>;
  }
  export interface Reply {
    send(body: unknown): void;
    forward(request: Request): void;
  }
  export type Handler = (request: Request, reply: Reply) => void;
  export interface Router {
    all(path: string, handler: Handler): void;
    get(path: string, handler: Handler): void;
  }
  export interface Server {
    use(prefix: string, router: Router): void;
  }
  export function createServer(): Server;
  export function createRouter(): Router;
}
