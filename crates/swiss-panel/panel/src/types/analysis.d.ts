export interface AnalysisRevision { id: string; parent: string | null; sql: string; conditions: Record<string, unknown>; note: string; at: string }
export interface AnalysisCheck { id: string; kind: string; state: string; by: string; note: string; measurement?: { result: string; rule: unknown; actual: number; expected: number; truncated: boolean } }
export interface AnalysisResult { id: string; revision: string; execution: string; rowCount: number; truncated: boolean; verification: string; note: string; checks: AnalysisCheck[]; superseded: boolean; chart: { kind: string; x: string; y: string[]; title: string } | null }
export interface AnalysisTask { id: string; title: string; conn: string; current: string; saved: boolean; revisions: AnalysisRevision[]; results: AnalysisResult[]; executions: { id: string; status: string; error: string | null; revision: string }[] }
export interface AnalysisPage { result: string; columns: string[]; rows: unknown[]; rowIds: string[]; offset: number; returned: number; total: number; more: boolean; truncated: boolean }
export interface AnalysisView { taskId: string; resultId: string; rowIds: string[] }
