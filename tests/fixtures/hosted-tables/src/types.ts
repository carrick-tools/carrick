export type Database = {
  public: {
    Tables: {
      accounts: { Row: { id: number; name: string } };
      notes: { Row: { id: number; body: string } };
      unused_table: { Row: { id: number } };
    };
    Views: {};
    Functions: {
      bump_count: { Args: {}; Returns: number };
    };
  };
};
