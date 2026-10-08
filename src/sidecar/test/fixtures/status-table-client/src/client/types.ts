// A generated HTTP client's result type: a conditional alias over the
// response table (`TData`) and the error table (`TError`). Each branch holds
// the table indexed by its own keys, beside the request and response objects
// the client hands back.
type Body<T> = T extends Record<string, unknown> ? T[keyof T] : T;

export type RequestResult<TData = unknown, TError = unknown, ThrowOnError extends boolean = boolean> =
  ThrowOnError extends true
    ? Promise<{ data: Body<TData>; request: Request; response: Response }>
    : Promise<
        (
          | { data: Body<TData>; error: undefined }
          | { data: undefined; error: Body<TError> }
        ) & { request: Request; response: Response }
      >;

export interface RequestOptions<ThrowOnError extends boolean = boolean> {
  url: string;
  path?: Record<string, unknown>;
  throwOnError?: ThrowOnError;
}

type MethodFn = <TData = unknown, TError = unknown, ThrowOnError extends boolean = false>(
  options: RequestOptions<ThrowOnError>
) => RequestResult<TData, TError, ThrowOnError>;

export interface Client {
  get: MethodFn;
  delete: MethodFn;
}

export declare const client: Client;
