// A stand-in for an HTTP client package's typings, kept in the repo.
declare module "http-kit" {
  export interface KitResponse<T> {
    status: number;
    data: T;
  }
}
