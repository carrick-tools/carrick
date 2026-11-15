// A default-exported registry of lazily loaded service modules.
const services = {
  web: () => import("./web"),
  jobs: () => import("./jobs"),
  timer: () => import("./timer"),
} as const;

export default services;
