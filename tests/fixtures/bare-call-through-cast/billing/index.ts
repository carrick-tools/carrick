import { formatTotal } from "../common/money";

export function invoice(c: number) {
  return formatTotal(c);
}
