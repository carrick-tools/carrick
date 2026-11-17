import { drop } from "./keep-generic-lib";
export const dropped = () => drop<{ lines: number }>("/ledger");
