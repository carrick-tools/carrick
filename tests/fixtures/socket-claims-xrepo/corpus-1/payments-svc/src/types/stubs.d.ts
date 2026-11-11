// Ambient stubs — keep types resolvable without npm install

declare module "express" {
  export interface Request {
    params: Record<string, string>;
    body: unknown;
    query: Record<string, string>;
  }
  export interface Response {
    status(code: number): Response;
    json(body: unknown): Response;
    send(body: unknown): Response;
  }
  export interface NextFunction {
    (err?: unknown): void;
  }
  export interface Router {
    get(path: string, handler: (req: Request, res: Response) => void): Router;
    post(path: string, handler: (req: Request, res: Response) => void): Router;
    use(handler: (req: Request, res: Response, next: NextFunction) => void): Router;
  }
  export interface Application {
    get(path: string, handler: (req: Request, res: Response) => void): Application;
    post(path: string, handler: (req: Request, res: Response) => void): Application;
    use(path: string, router: Router): Application;
    use(handler: (req: Request, res: Response, next: NextFunction) => void): Application;
    listen(port: number, callback?: () => void): void;
  }
  function express(): Application;
  export default express;
}

declare module "axios" {
  export interface AxiosResponse<T = unknown> {
    data: T;
    status: number;
    statusText: string;
  }
  export interface AxiosInstance {
    get<T = unknown>(url: string, config?: unknown): Promise<AxiosResponse<T>>;
    post<T = unknown>(url: string, data?: unknown, config?: unknown): Promise<AxiosResponse<T>>;
  }
  const axios: AxiosInstance;
  export default axios;
}

// AWS SDK stubs — referenced only by the decoy in lib/audit.ts
declare module "@aws-sdk/client-dynamodb" {
  export class DynamoDBClient {
    send(command: unknown): Promise<unknown>;
  }
  export class PutItemCommand {
    constructor(input: unknown);
  }
}

declare module "http" {
  export interface HttpServer {
    listen(port: number, callback?: () => void): void;
  }
  export function createServer(): HttpServer;
}
