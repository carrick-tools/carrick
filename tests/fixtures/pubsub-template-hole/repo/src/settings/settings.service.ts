import { Injectable } from "../framework";
import { BusService } from "../events/bus.service";

export enum SettingName {
  MAIL_ENABLED = "MAIL_ENABLED",
  AUDIT_ENABLED = "AUDIT_ENABLED",
}

@Injectable()
export class SettingsService {
  constructor(private readonly bus: BusService) {}

  async toggleMail(on: boolean) {
    return this.toggleSetting(SettingName.MAIL_ENABLED, on);
  }

  async toggleSetting(name: SettingName, on: boolean) {
    this.bus.publish(
      `setting/${name}/changed`,
      on,
    );
    return true;
  }
}
