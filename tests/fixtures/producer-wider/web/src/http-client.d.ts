declare module "http-client" {
  export interface ClientResponse<T = any> {
    data: T;
    status: number;
  }
  export interface ClientInstance {
    get<T = any, R = ClientResponse<T>>(url: string): Promise<R>;
  }
  export function create(): ClientInstance;
}
