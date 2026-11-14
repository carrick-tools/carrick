import { probe } from "./own-generic-lib";
export const probed = () => probe<{ alive: boolean }>("/probes");
