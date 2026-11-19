export interface Project {
  ref: string;
}
export interface Run {
  code: string;
  project: Project;
}
export declare function loadRun(id: string): Promise<Run>;
