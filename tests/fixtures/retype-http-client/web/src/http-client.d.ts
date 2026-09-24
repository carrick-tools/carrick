declare module "http-client" {
  export interface RequestOptions<D = any> {
    baseURL?: string;
    data?: D;
  }
  export interface ClientResponse<T = any, D = any> {
    data: T;
    status: number;
    options: RequestOptions<D>;
  }
  export interface ClientInstance {
    post<T = any, R = ClientResponse<T>, D = any>(url: string, data?: D): Promise<R>;
  }
  export function create(options?: RequestOptions): ClientInstance;
}
