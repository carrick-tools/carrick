import { createTransport } from "postbox-mailer";

export const buildTransport = (host: string) => {
  if (host.length === 0) {
    return createTransport({ jsonTransport: true });
  }

  return createTransport({ host, port: 587 });
};
