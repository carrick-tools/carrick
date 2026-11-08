import { http } from "@fixture/http";

class Rpc {
  call(path: string) {
    return http.post(path, null);
  }
}

const rpc = new Rpc();

export function f() {
  return rpc.call("/p/m-rpc-call");
}
