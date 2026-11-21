// A registry of lazily loaded service modules: each entry is a thunk that
// imports its module when called.
const services = {
  web: () => import("./web"),
  admin: () => import("./admin"),
} as const;

export default services;
