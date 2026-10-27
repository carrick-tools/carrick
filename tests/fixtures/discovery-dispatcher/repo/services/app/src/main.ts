import { connect, fetchOne } from "@fixture/client";

export function viaOauth() {
  return connect({ type: "OAUTH", key: "acme" });
}

export function viaJwt() {
  return connect({ type: "JWT", key: "acme" });
}

export function viaMode(mode: "OAUTH" | "TWO_STEP" | "JWT") {
  return connect({ type: mode, key: "acme" });
}

export function one() {
  return fetchOne("42");
}
