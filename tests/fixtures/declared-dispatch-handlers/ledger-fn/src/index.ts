export interface Ctx {
  requestId: string;
}

export interface CreditCommand {
  op: "credit";
  account: string;
  amount: number;
}

export interface DebitCommand {
  op: "debit";
  account: string;
  amount: number;
  reason: string;
}

export interface CreditResult {
  account: string;
  balance: number;
}

export interface DebitResult {
  account: string;
  balance: number;
  overdrawn: boolean;
}

type LedgerCommand = CreditCommand | DebitCommand;

interface LambdaEvent {
  body: string | null;
  requestContext: { requestId: string };
}

export async function credit(cmd: CreditCommand, ctx: Ctx): Promise<CreditResult> {
  console.log(ctx.requestId);
  return { account: cmd.account, balance: cmd.amount };
}

const debit = async (cmd: DebitCommand, ctx: Ctx): Promise<DebitResult> => {
  console.log(ctx.requestId, cmd.reason);
  return { account: cmd.account, balance: -cmd.amount, overdrawn: cmd.amount > 0 };
};

export async function handler(event: LambdaEvent): Promise<CreditResult | DebitResult> {
  const cmd = JSON.parse(event.body ?? "{}") as LedgerCommand;
  const ctx: Ctx = { requestId: event.requestContext.requestId };
  switch (cmd.op) {
    case "credit":
      return credit(cmd, ctx);
    case "debit":
      return debit(cmd, ctx);
  }
}
