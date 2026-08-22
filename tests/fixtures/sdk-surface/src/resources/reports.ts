export class Reports {
  constructor(private readonly client?: unknown) {}

  monthly = async (month: string) => fetch(`/v1/reports/${month}`);
}
