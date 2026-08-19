import { buildTransport } from "./transport";

const getTransport = () => buildTransport(process.env.MAIL_HOST ?? "");

export const mailer = getTransport();

export const backupMailer = buildTransport("backup.invalid");

export const verifyTransport = () => getTransport().verify();

export type { Transporter } from "postbox-mailer";
