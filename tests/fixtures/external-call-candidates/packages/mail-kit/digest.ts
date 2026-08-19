import { createTransport } from "postbox-mailer";

export default function digestTransport() {
  return createTransport({ jsonTransport: true });
}
