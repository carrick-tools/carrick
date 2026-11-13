declare module "http-server" {
  export interface Request<Body = unknown> {
    params: Record<string, string>;
    body: Body;
  }
  export interface Reply<Result = unknown> {
    json(body: Result): void;
    forward(request: Request<any>): void;
  }
  export interface Server {
    all(path: string, handler: (request: Request<any>, reply: Reply<any>) => void): void;
    post<Body, Result>(
      path: string,
      handler: (request: Request<Body>, reply: Reply<Result>) => void
    ): void;
  }
  export function createServer(): Server;
}
