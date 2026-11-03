export interface AnalysisRevision { id: string; parent: string | null; sql: string; conditions: unknown; params: unknown; note: string; at: string }
export interface AnalysisCheck { id: string; at: string; kind: string; state: string; by: string; note: string; evidence?: string; measurement?: { result: string; rule: unknown; actual: number; expected: number; truncated: boolean } }
export interface AnalysisResult { id: string; revision: string; execution: string; at: string; columns: string[]; elapsedMs: number; rowCount: number; truncated: boolean; verification: string; note: string; checks: AnalysisCheck[]; superseded: boolean; chart: { kind: string; x: string; y: string[]; title: string } | null }
export interface AnalysisExecution { id: string; status: string; sql: string; params: unknown; error: string | null; revision: string; startedAt: string; finishedAt: string | null; elapsedMs: number; superseded: boolean; rows: number; truncated: boolean; result: string | null }
export interface AnalysisTask { id: string; title: string; conn: string; kind: string; current: string; saved: boolean; createdAt: string; updatedAt: string; revisions: AnalysisRevision[]; results: AnalysisResult[]; executions: AnalysisExecution[] }
export interface AnalysisPage { result: string; columns: string[]; rows: unknown[]; rowIds: string[]; offset: number; returned: number; total: number; more: boolean; truncated: boolean }
export interface AnalysisView { taskId: string; resultId: string; rowIds: string[] }
export interface AnalysisComparison {
  left: { result: string; revision: string; at: string; rows: number; truncated: boolean };
  right: AnalysisComparison["left"];
  keys: string[]; added: number; removed: number; changed: number; unchanged: number;
  columnsSame: boolean; columns: { onlyLeft: string[]; onlyRight: string[] };
  keyIssues: { left: { duplicateRows: number; duplicateKeys: number; nullRows: number }; right: { duplicateRows: number; duplicateKeys: number; nullRows: number } };
  changedSample: { key: unknown; fields: { column: string; left: unknown; right: unknown; leftMissing: boolean; rightMissing: boolean }[] }[];
  addedSample: { key: unknown; row: unknown; count: number }[]; removedSample: AnalysisComparison["addedSample"];
}
