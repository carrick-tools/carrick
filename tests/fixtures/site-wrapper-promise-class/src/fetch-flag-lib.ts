export async function sent(b: string) {
  await fetch(`${b}/flag`);
  return true;
}
