const API_BASE = import.meta.env.VITE_API_BASE || 'http://localhost:18790';

async function request<T>(path: string, options?: RequestInit): Promise<T> {
  const url = `${API_BASE}/v1${path}`;
  const token = localStorage.getItem('blockcell_token');
  const headers: Record<string, string> = {
    'Content-Type': 'application/json',
    ...(options?.headers as Record<string, string>),
  };
  if (token) {
    headers['Authorization'] = `Bearer ${token}`;
  }
  const res = await fetch(url, { ...options, headers });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(`API ${res.status}: ${text}`);
  }
  return res.json();
}

// Auth
export async function login(password: string): Promise<{ token?: string; error?: string }> {
  const url = `${API_BASE}/v1/auth/login`;
  const res = await fetch(url, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ password }),
  });
  return res.json();
}

export function logout() {
  localStorage.removeItem('blockcell_token');
  window.location.reload();
}

// P0: Chat
export function sendChat(content: string, chatId = 'default', media: string[] = []) {
  return request<{ status: string; message: string }>('/chat', {
    method: 'POST',
    body: JSON.stringify({ content, chat_id: chatId, channel: 'ws', media }),
  });
}

// P0: Health
export function getHealth() {
  return request<{ status: string; model: string; uptime_secs: number; version: string }>('/health');
}

// P0: Tasks
export function getTasks() {
  return request<{ queued: number; running: number; completed: number; failed: number; tasks: any[] }>('/tasks');
}

// P0: Sessions
export function getSessions() {
  return request<{ sessions: SessionInfo[]; next_cursor: number | null; total: number }>('/sessions');
}

export function getSessionsPage(params?: { limit?: number; cursor?: number }) {
  const qs = new URLSearchParams();
  if (params?.limit !== undefined) qs.set('limit', String(params.limit));
  if (params?.cursor !== undefined) qs.set('cursor', String(params.cursor));
  const suffix = qs.toString();
  return request<{ sessions: SessionInfo[]; next_cursor: number | null; total: number }>(`/sessions${suffix ? `?${suffix}` : ''}`);
}

export function getSession(id: string) {
  return request<{ session_id: string; messages: ChatMsg[] }>(`/sessions/${id}`);
}

export function deleteSession(id: string) {
  return request<{ status: string }>(`/sessions/${id}`, { method: 'DELETE' });
}

export function renameSession(id: string, name: string) {
  return request<{ status: string }>(`/sessions/${id}/rename`, {
    method: 'PUT',
    body: JSON.stringify({ name }),
  });
}

// P1: Config
export function getConfig() {
  return request<any>('/config');
}

export function updateConfig(config: any) {
  return request<{ status: string; message: string }>('/config', {
    method: 'PUT',
    body: JSON.stringify(config),
  });
}

export function testProvider(params: { model: string; api_key: string; api_base?: string }) {
  return request<{ status: string; message: string }>('/config/test-provider', {
    method: 'POST',
    body: JSON.stringify(params),
  });
}

// P1: Memory
export function getMemories(params?: { q?: string; scope?: string; type?: string; limit?: number }) {
  const qs = new URLSearchParams();
  if (params?.q) qs.set('q', params.q);
  if (params?.scope) qs.set('scope', params.scope);
  if (params?.type) qs.set('type', params.type);
  if (params?.limit) qs.set('limit', String(params.limit));
  return request<any>(`/memory?${qs}`);
}

export function createMemory(data: any) {
  return request<any>('/memory', { method: 'POST', body: JSON.stringify(data) });
}

export function deleteMemory(id: string) {
  return request<any>(`/memory/${id}`, { method: 'DELETE' });
}

export function getMemoryStats() {
  return request<any>('/memory/stats');
}

// P1: Tools / Skills / Evolution / Stats
export function getTools() {
  return request<{ tools: any[]; count: number }>('/tools');
}

export function getSkills() {
  return request<{ skills: any[]; count: number }>('/skills');
}

export function searchSkills(query: string) {
  return request<{ results: any[]; count: number; query: string }>('/skills/search', {
    method: 'POST',
    body: JSON.stringify({ query }),
  });
}

export function getEvolution() {
  return request<{ records: any[]; count: number }>('/evolution');
}

export function getEvolutionDetail(id: string) {
  return request<{ record: EvolutionRecord; kind: string }>(`/evolution/${id}`);
}

export function getEvolutionToolEvolutions() {
  return request<{ records: CoreEvolutionRecord[]; count: number }>('/evolution/tool-evolutions');
}

export function triggerEvolution(skillName: string, description: string) {
  return request<{ status: string; evolution_id?: string; error?: string }>('/evolution/trigger', {
    method: 'POST',
    body: JSON.stringify({ skill_name: skillName, description }),
  });
}

export function deleteEvolution(id: string) {
  return request<{ status: string }>(`/evolution/${id}`, { method: 'DELETE' });
}

export function testSkill(skillName: string, input: string) {
  return request<{ status: string; skill_name: string; result?: string; error?: string; duration_ms?: number }>('/evolution/test', {
    method: 'POST',
    body: JSON.stringify({ skill_name: skillName, input }),
  });
}

export function getTestSuggestion(skillName: string) {
  return request<{ skill_name: string; suggestion?: string; error?: string }>('/evolution/test-suggest', {
    method: 'POST',
    body: JSON.stringify({ skill_name: skillName }),
  });
}

export function getSkillVersions(skillName: string) {
  return request<{ versions: any[]; current_version: string }>(`/evolution/versions/${skillName}`);
}

export function getToolEvolutionVersions(toolId: string) {
  return request<{ capability_id: string; versions: any[]; current_version: string }>(`/evolution/tool-versions/${toolId}`);
}

export function getEvolutionSummary() {
  return request<EvolutionSummary>('/evolution/summary');
}

export function getStats() {
  return request<any>('/stats');
}

// P1: Cron
export function getCronJobs() {
  return request<{ jobs: any[]; count: number }>('/cron');
}

export function createCronJob(data: any) {
  return request<any>('/cron', { method: 'POST', body: JSON.stringify(data) });
}

export function deleteCronJob(id: string) {
  return request<any>(`/cron/${id}`, { method: 'DELETE' });
}

export function runCronJob(id: string) {
  return request<any>(`/cron/${id}/run`, { method: 'POST' });
}

// P2: Alerts
export function getAlerts() {
  return request<{ rules: AlertRule[]; count: number }>('/alerts');
}

export function createAlert(data: Partial<AlertRule>) {
  return request<{ status: string; rule_id: string }>('/alerts', {
    method: 'POST',
    body: JSON.stringify(data),
  });
}

export function updateAlert(id: string, data: Partial<AlertRule>) {
  return request<{ status: string }>(`/alerts/${id}`, {
    method: 'PUT',
    body: JSON.stringify(data),
  });
}

export function deleteAlert(id: string) {
  return request<{ status: string }>(`/alerts/${id}`, { method: 'DELETE' });
}

export function getAlertHistory() {
  return request<{ history: AlertHistoryEntry[] }>('/alerts/history');
}

// P2: Streams
export function getStreams() {
  return request<{ streams: StreamInfo[]; count: number }>('/streams');
}

export function getStreamData(id: string, limit = 50) {
  return request<any>(`/streams/${id}/data?limit=${limit}`);
}

// Toggles
export function getToggles() {
  return request<{ skills: Record<string, boolean>; tools: Record<string, boolean> }>('/toggles');
}

export function updateToggle(category: 'skills' | 'tools', name: string, enabled: boolean) {
  return request<{ status: string; category: string; name: string; enabled: boolean }>('/toggles', {
    method: 'PUT',
    body: JSON.stringify({ category, name, enabled }),
  });
}

// P2: Files
export function getFiles(path = '.') {
  return request<{ path: string; entries: FileEntry[]; count: number }>(`/files?path=${encodeURIComponent(path)}`);
}

export function getFileContent(path: string) {
  return request<FileContent>(`/files/content?path=${encodeURIComponent(path)}`);
}

export function downloadFileUrl(path: string) {
  const token = localStorage.getItem('blockcell_token');
  const base = `${API_BASE}/v1/files/download?path=${encodeURIComponent(path)}`;
  return token ? `${base}&token=${token}` : base;
}

export function mediaFileUrl(path: string) {
  const token = localStorage.getItem('blockcell_token');
  const base = `${API_BASE}/v1/files/serve?path=${encodeURIComponent(path)}`;
  return token ? `${base}&token=${token}` : base;
}

export function uploadFile(path: string, content: string, encoding: 'utf-8' | 'base64' = 'utf-8') {
  return request<{ status: string; path: string }>('/files/upload', {
    method: 'POST',
    body: JSON.stringify({ path, content, encoding }),
  });
}

// Evolution types
export interface EvolutionSummary {
  skill_evolution: { total: number; active: number; completed: number; failed: number };
  tool_evolution: { total: number; active: number; completed: number; failed: number };
  inventory: { user_skills: number; builtin_skills: number; registered_tools: number };
}

export interface EvolutionRecord {
  id: string;
  skill_name: string;
  context: {
    skill_name: string;
    current_version: string;
    trigger: any;
    error_stack?: string;
    source_snippet?: string;
    tool_schemas: any[];
    timestamp: number;
  };
  patch?: {
    diff: string;
    explanation: string;
    generated_at: number;
  };
  audit?: {
    passed: boolean;
    issues: { severity: string; category: string; message: string }[];
    audited_at: number;
  };
  shadow_test?: {
    passed: boolean;
    test_cases_run: number;
    test_cases_passed: number;
    errors: string[];
    tested_at: number;
  };
  rollout?: {
    stages: { percentage: number; duration_minutes: number; error_threshold: number }[];
    current_stage: number;
    started_at: number;
  };
  status: string;
  attempt: number;
  feedback_history: {
    attempt: number;
    stage: string;
    feedback: string;
    previous_code: string;
    timestamp: number;
  }[];
  created_at: number;
  updated_at: number;
}

export interface CoreEvolutionRecord {
  id: string;
  capability_id: string;
  description: string;
  status: string;
  provider_kind: string;
  source_code?: string;
  artifact_path?: string;
  compile_output?: string;
  validation?: {
    passed: boolean;
    checks: { name: string; passed: boolean; message: string }[];
  };
  attempt: number;
  feedback_history: {
    attempt: number;
    stage: string;
    feedback: string;
    previous_code: string;
    timestamp: number;
  }[];
  input_schema?: any;
  output_schema?: any;
  created_at: number;
  updated_at: number;
}

// Types
export interface AlertRule {
  id: string;
  name: string;
  enabled: boolean;
  source: any;
  metric_path: string;
  operator: string;
  threshold: number;
  threshold2?: number;
  cooldown_secs: number;
  check_interval_secs: number;
  notify: { channel: string; template?: string; params?: any };
  on_trigger: any[];
  state: {
    last_value?: number;
    prev_value?: number;
    last_check_at?: number;
    last_triggered_at?: number;
    trigger_count: number;
    last_error?: string;
  };
  created_at: number;
  updated_at: number;
}

export interface AlertHistoryEntry {
  rule_id: string;
  name: string;
  trigger_count: number;
  last_triggered_at?: number;
  last_value?: number;
  threshold?: number;
  operator?: string;
}

export interface StreamInfo {
  stream_id: string;
  url: string;
  protocol: string;
  status: string;
  message_count: number;
  buffered: number;
  created_at: number;
  last_message_at?: number;
  error?: string;
  auto_restore: boolean;
  reconnect_count: number;
}

export interface FileEntry {
  name: string;
  path: string;
  is_dir: boolean;
  size: number;
  type: string;
  modified?: string;
}

export interface FileContent {
  path: string;
  encoding: string;
  mime_type: string;
  size: number;
  content: string;
}

// Ghost Agent
export interface GhostConfig {
  enabled: boolean;
  model: string | null;
  schedule: string;
  maxSyncsPerDay: number;
  autoSocial: boolean;
}

export interface GhostActivity {
  session_id: string;
  timestamp: string;
  message_count: number;
  routine_prompt: string;
  summary: string;
  tool_calls: string[];
}

export interface GhostModelOptions {
  providers: string[];
  default_model: string;
}

export function getGhostConfig() {
  return request<GhostConfig>('/ghost/config');
}

export function updateGhostConfig(config: Partial<GhostConfig>) {
  return request<{ status: string; message: string; config?: GhostConfig }>('/ghost/config', {
    method: 'PUT',
    body: JSON.stringify(config),
  });
}

export function getGhostActivity(limit = 20) {
  return request<{ activities: GhostActivity[]; count: number }>(`/ghost/activity?limit=${limit}`);
}

export function getGhostModelOptions() {
  return request<GhostModelOptions>('/ghost/model-options');
}

export interface SessionInfo {
  id: string;
  name: string;
  updated_at: string;
  message_count: number;
}

export interface ChatMsg {
  role: string;
  content: any;
  tool_calls?: any[];
  tool_call_id?: string;
  reasoning_content?: string;
}
