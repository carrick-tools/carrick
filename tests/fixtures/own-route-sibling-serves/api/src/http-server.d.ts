declare module "http-server" {
  export interface Request {
    params: Record<string, string>;
  }
  export interface Reply {
    json(body: unknown): void;
    forward(request: Request): void;
  }
  export interface Server {
    get(path: string, handler: (request: Request, reply: Reply) => void): void;
  }
  export function createServer(): Server;
}
