export type NoticeScope = "all" | "specific";

export class CreateNoticeDto {
  scope!: NoticeScope;
  userIds?: string[];
  body!: string;
}
