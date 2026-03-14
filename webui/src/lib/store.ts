import { create } from 'zustand';
import type { SessionInfo, ChatMsg } from './api';
import type { WsEvent, ConnectionState, DisconnectReason } from './ws';
import { notifyTaskCompleted, notifyAlertTriggered, notifySystemEvent } from './notifications';

function normalizeSessionId(id: string) {
  return id.replace(/:/g, '_');
}

function looksLikeReminderMessage(content?: string) {
  if (!content) return false;
  return content.startsWith('⏰') || content.includes('提醒') || content.includes('该起床了');
}

function looksLikeBackgroundDeliveryMessage(content?: string) {
  if (!content) return false;
  return content.startsWith('⏰')
    || content.includes('提醒')
    || content.includes('定时任务')
    || content.includes('执行失败')
    || content.includes('新闻');
}

function containsToolTraceContent(content?: string) {
  const trimmed = (content || '').trim();
  if (!trimmed) return false;

  const lower = trimmed.toLowerCase();
  return lower.includes('<tool_call')
    || lower.includes('[tool_call]')
    || lower.includes('[/tool_call]')
    || lower.includes('[called:');
}

function isCronBackgroundDelivery(event: WsEvent) {
  return event.background_delivery === true && event.delivery_kind === 'cron';
}

function isLikelyCronJobId(id: string) {
  return /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(id);
}

function isValidReminderSessionId(id: string) {
  if (!id) return false;
  if (isLikelyCronJobId(id)) return false;
  return id.includes('_') || id.includes(':');
}

function buildReminderPreview(content?: string) {
  const raw = (content || '').trim();
  if (!raw) return '点击查看提醒';

  const firstLine = raw
    .split(/\r?\n/)
    .map((line) => line.trim())
    .find((line) => line.length > 0);
  const candidate = (firstLine || raw).replace(/\s+/g, ' ').trim();

  return candidate.length > 96 ? `${candidate.slice(0, 96).trimEnd()}...` : candidate;
}

function ensureSessionExists(state: ChatState, sessionId: string): SessionInfo[] {
  if (state.sessions.some((s) => s.id === sessionId)) {
    return state.sessions;
  }

  return [
    {
      id: sessionId,
      name: sessionId,
      message_count: 0,
      updated_at: new Date().toISOString(),
    },
    ...state.sessions,
  ];
}

function promoteSession(state: ChatState, sessionId: string, name?: string): SessionInfo[] {
  const existing = state.sessions.find((s) => s.id === sessionId);
  const next: SessionInfo = existing
    ? { ...existing, name: name || existing.name, updated_at: new Date().toISOString() }
    : {
        id: sessionId,
        name: name || sessionId,
        message_count: 0,
        updated_at: new Date().toISOString(),
      };

  return [next, ...state.sessions.filter((s) => s.id !== sessionId)];
}

function sessionStorageKey(agentId: string) {
  return `blockcell_last_session_id:${agentId}`;
}

// ── Chat message with UI metadata ──
export interface UiMessage {
  id: string;
  role: string;
  content: string;
  toolCalls?: ToolCallInfo[];
  reasoning?: string;
  timestamp: number;
  streaming?: boolean;
  media?: string[];
  highlight?: boolean;
}

export interface ToolCallInfo {
  id: string;
  tool: string;
  params: any;
  result?: any;
  durationMs?: number;
  status: 'running' | 'done' | 'error';
}

export interface ReminderAlertItem {
  id: string;
  sessionId: string;
  agentId: string;
  preview: string;
  content: string;
  timestamp: number;
}

// ── Chat Store ──
interface ChatState {
  sessions: SessionInfo[];
  currentSessionId: string;
  messages: UiMessage[];
  isConnected: boolean;
  isLoading: boolean;
  pendingFocusSessionId?: string;
  pendingFocusText?: string;

  setSessions: (sessions: SessionInfo[]) => void;
  setCurrentSession: (id: string) => void;
  setMessages: (messages: UiMessage[]) => void;
  addMessage: (msg: UiMessage) => void;
  updateLastAssistantMessage: (fn: (msg: UiMessage) => UiMessage) => void;
  setConnected: (v: boolean) => void;
  setLoading: (v: boolean) => void;
  setPendingReminderFocus: (sessionId?: string, text?: string) => void;

  // WS event handlers
  handleWsEvent: (event: WsEvent) => void;
}

let msgCounter = 0;
function nextMsgId() {
  return `msg_${Date.now()}_${++msgCounter}`;
}

function resolveInitialSessionId(agentId: string): string {
  if (typeof window === 'undefined') return '';
  const saved = localStorage.getItem(sessionStorageKey(agentId));
  return saved || '';
}

export const useChatStore = create<ChatState>((set, get) => ({
  sessions: [],
  currentSessionId: resolveInitialSessionId(
    typeof window === 'undefined' ? 'default' : (localStorage.getItem('blockcell_selected_agent') || 'default')
  ),
  messages: [],
  isConnected: false,
  isLoading: false,
  pendingFocusSessionId: undefined,
  pendingFocusText: undefined,

  setSessions: (sessions) => set({ sessions }),
  setCurrentSession: (id) => {
    if (typeof window !== 'undefined') {
      const agentId = localStorage.getItem('blockcell_selected_agent') || 'default';
      if (id) {
        localStorage.setItem(sessionStorageKey(agentId), id);
      } else {
        localStorage.removeItem(sessionStorageKey(agentId));
      }
    }
    set({ currentSessionId: id, messages: [] });
  },
  setMessages: (messages) => set({ messages }),
  addMessage: (msg) => set((s) => ({ messages: [...s.messages, msg] })),
  updateLastAssistantMessage: (fn) =>
    set((s) => {
      const msgs = [...s.messages];
      for (let i = msgs.length - 1; i >= 0; i--) {
        if (msgs[i].role === 'assistant') {
          msgs[i] = fn(msgs[i]);
          break;
        }
      }
      return { messages: msgs };
    }),
  setConnected: (v) => set({ isConnected: v }),
  setLoading: (v) => set({ isLoading: v }),
  setPendingReminderFocus: (sessionId, text) =>
    set({ pendingFocusSessionId: sessionId, pendingFocusText: text }),

  handleWsEvent: (event) => {
    const state = get();
    const selectedAgentId = typeof window === 'undefined'
      ? 'default'
      : (localStorage.getItem('blockcell_selected_agent') || 'default');

    if (event.type === 'message_done' && event.chat_id) {
      if (event.agent_id && event.agent_id !== selectedAgentId) {
        return;
      }

      const normalizedEventChatId = normalizeSessionId(event.chat_id);
      const normalizedCurrentSessionId = normalizeSessionId(state.currentSessionId);
      if (normalizedEventChatId !== normalizedCurrentSessionId) {
        if ((isCronBackgroundDelivery(event) || looksLikeBackgroundDeliveryMessage(event.content)) && isValidReminderSessionId(event.chat_id)) {
          useReminderAlertsStore.getState().pushAlert({
            id: `reminder-${event.chat_id}-${Date.now()}`,
            sessionId: normalizedEventChatId,
            agentId: selectedAgentId,
            preview: buildReminderPreview(event.content),
            content: event.content || '点击查看提醒',
            timestamp: Date.now(),
          });
        }
        return;
      }
    }

    // Filter chat-specific events by both agent_id and chat_id to prevent
    // cross-agent and cross-session leaking.
    const chatEventTypes: string[] = ['message_done', 'token', 'tool_call_start', 'tool_call_result', 'thinking'];
    if (chatEventTypes.includes(event.type) && event.chat_id) {
      if (event.agent_id && event.agent_id !== selectedAgentId) {
        return;
      }
      if (normalizeSessionId(event.chat_id) !== normalizeSessionId(state.currentSessionId)) {
        return; // Event belongs to a different chat session — ignore
      }
    }

    switch (event.type) {
      case 'session_bound': {
        if (!event.chat_id) {
          break;
        }
        if (event.agent_id && event.agent_id !== selectedAgentId) {
          break;
        }

        const normalizedRealId = normalizeSessionId(event.chat_id);
        const normalizedClientId = normalizeSessionId(event.client_chat_id || '');

        set((innerState) => {
          const sessionsWithoutClient = normalizedClientId
            ? innerState.sessions.filter((s) => s.id !== normalizedClientId && s.id !== normalizedRealId)
            : innerState.sessions.filter((s) => s.id !== normalizedRealId);

          const promoted = promoteSession({ ...innerState, sessions: sessionsWithoutClient }, normalizedRealId);
          const shouldSwitchCurrent =
            !innerState.currentSessionId
            || !normalizedClientId
            || normalizeSessionId(innerState.currentSessionId) === normalizedClientId;

          return {
            sessions: promoted,
            currentSessionId: shouldSwitchCurrent ? normalizedRealId : innerState.currentSessionId,
          };
        });
        break;
      }

      case 'session_renamed': {
        if (event.chat_id && event.name) {
          if (event.agent_id && event.agent_id !== selectedAgentId) {
            break;
          }
          const normalizedId = normalizeSessionId(event.chat_id);
          
          set((state) => {
            return {
              sessions: promoteSession(state, normalizedId, event.name!),
            };
          });
        }
        break;
      }

      case 'message_done': {
        // Check if there's a streaming assistant message to finalize
        const lastMsg = state.messages[state.messages.length - 1];
        const hasExplicitContent = Object.prototype.hasOwnProperty.call(event, 'content');
        const finalContent = hasExplicitContent ? (event.content ?? '') : undefined;
        const shouldDropStreamingToolTrace =
          lastMsg?.role === 'assistant'
          && lastMsg.streaming
          && containsToolTraceContent(lastMsg.content)
          && (!finalContent || !finalContent.trim())
          && !(event.media && event.media.length > 0);
        const highlight =
          state.pendingFocusSessionId === state.currentSessionId
          && !!state.pendingFocusText
          && (event.content || '').includes(state.pendingFocusText);
        if (shouldDropStreamingToolTrace) {
          set((s) => ({ messages: s.messages.slice(0, -1) }));
        } else if (lastMsg?.role === 'assistant' && lastMsg.streaming) {
          state.updateLastAssistantMessage((m) => ({
            ...m,
            content: finalContent ?? m.content,
            streaming: false,
            media: event.media && event.media.length > 0
              ? [...new Set([...(m.media || []), ...event.media])]
              : m.media,
          }));
        } else if (
          lastMsg?.role === 'assistant'
          && !lastMsg.streaming
          && (lastMsg.content || '') === (event.content || '')
        ) {
          state.updateLastAssistantMessage((m) => ({
            ...m,
            media: event.media && event.media.length > 0
              ? [...new Set([...(m.media || []), ...event.media])]
              : m.media,
            highlight: m.highlight || highlight,
          }));
        } else {
          // New complete message
          state.addMessage({
            id: nextMsgId(),
            role: 'assistant',
            content: event.content || '',
            timestamp: Date.now(),
            streaming: false,
            media: event.media && event.media.length > 0 ? event.media : undefined,
            highlight,
          });
        }
        if (
          state.pendingFocusSessionId === state.currentSessionId
          && state.pendingFocusText
          && (event.content || '').includes(state.pendingFocusText)
        ) {
          state.setPendingReminderFocus(undefined, undefined);
        }
        set({ isLoading: false });
        break;
      }

      case 'token': {
        // 直接使用 set() 并在回调中获取最新状态，确保流式追加正确
        set((s) => {
          const lastMsg = s.messages[s.messages.length - 1];
          if (lastMsg?.role === 'assistant' && lastMsg.streaming) {
            const msgs = [...s.messages];
            for (let i = msgs.length - 1; i >= 0; i--) {
              if (msgs[i].role === 'assistant') {
                msgs[i] = { ...msgs[i], content: msgs[i].content + (event.delta || '') };
                break;
              }
            }
            return { messages: msgs };
          } else {
            return {
              messages: [...s.messages, {
                id: nextMsgId(),
                role: 'assistant',
                content: event.delta || '',
                timestamp: Date.now(),
                streaming: true,
              }],
            };
          }
        });
        break;
      }

      case 'thinking': {
        // 直接使用 set() 并在回调中获取最新状态，确保流式追加正确
        set((s) => {
          const lastMsg = s.messages[s.messages.length - 1];
          if (lastMsg?.role === 'assistant' && lastMsg.streaming) {
            const msgs = [...s.messages];
            for (let i = msgs.length - 1; i >= 0; i--) {
              if (msgs[i].role === 'assistant') {
                msgs[i] = { ...msgs[i], reasoning: (msgs[i].reasoning || '') + (event.content || '') };
                break;
              }
            }
            return { messages: msgs };
          }
          return {}; // 无变化
        });
        break;
      }

      case 'tool_call_start': {
        const lastMsg = state.messages[state.messages.length - 1];
        const toolCall: ToolCallInfo = {
          id: event.call_id || '',
          tool: event.tool || '',
          params: event.params,
          status: 'running',
        };
        if (lastMsg?.role === 'assistant') {
          state.updateLastAssistantMessage((m) => ({
            ...m,
            toolCalls: [...(m.toolCalls || []), toolCall],
          }));
        } else {
          // No assistant message yet — create one to hold tool calls
          state.addMessage({
            id: nextMsgId(),
            role: 'assistant',
            content: '',
            toolCalls: [toolCall],
            timestamp: Date.now(),
            streaming: true,
          });
        }
        break;
      }

      case 'tool_call_result': {
        state.updateLastAssistantMessage((m) => ({
          ...m,
          toolCalls: (m.toolCalls || []).map((tc) =>
            tc.id === event.call_id
              ? { ...tc, result: event.result, durationMs: event.duration_ms, status: 'done' as const }
              : tc
          ),
        }));
        break;
      }

      case 'task_update': {
        // Send browser notification for task completion
        if (event.status === 'Completed' || event.status === 'Failed') {
          notifyTaskCompleted(event.label || event.task_id || 'Task', event.status === 'Completed');
        }
        break;
      }

      case 'alert_triggered': {
        // Send browser notification for alert
        notifyAlertTriggered(event.alert_name || 'Alert', event.alert_value);
        // Also show in chat as a system message
        state.addMessage({
          id: nextMsgId(),
          role: 'assistant',
          content: `🔔 Alert triggered: **${event.alert_name || 'Unknown'}**${event.alert_value !== undefined ? ` — Value: ${event.alert_value}` : ''}`,
          timestamp: Date.now(),
        });
        break;
      }

      case 'error': {
        state.addMessage({
          id: nextMsgId(),
          role: 'assistant',
          content: `❌ Error: ${event.message}`,
          timestamp: Date.now(),
        });
        set({ isLoading: false });
        break;
      }

      case 'system_event_notification': {
        const sysStore = useSystemEventsStore.getState();
        sysStore.addEvent({
          id: event.event_id || `evt_${Date.now()}`,
          kind: 'notification',
          priority: event.priority || 'Normal',
          title: event.title || 'System Event',
          body: event.body || '',
          timestamp: Date.now(),
          agentId: event.agent_id,
        });
        if (event.priority === 'Critical' || event.priority === 'High') {
          notifySystemEvent(event.title || 'System Event', event.body || '', event.priority);
        }
        break;
      }

      case 'system_event_summary': {
        const sysStore2 = useSystemEventsStore.getState();
        const summaryBody = event.compact_text || (event.items || []).map((i: any) => i.title || i.body).join('\n');
        sysStore2.addEvent({
          id: `summary_${Date.now()}`,
          kind: 'summary',
          priority: 'Normal',
          title: event.title || 'Session Summary',
          body: summaryBody,
          timestamp: Date.now(),
          agentId: event.agent_id,
          items: event.items,
        });
        break;
      }
    }
  },
}));

// ── System Events Store ──
export interface SystemEventItem {
  id: string;
  kind: 'notification' | 'summary';
  priority: string;
  title: string;
  body: string;
  timestamp: number;
  agentId?: string;
  items?: any[];
  read?: boolean;
}

interface SystemEventsState {
  events: SystemEventItem[];
  unreadCount: number;
  addEvent: (event: SystemEventItem) => void;
  markAllRead: () => void;
  clearAll: () => void;
}

const MAX_SYSTEM_EVENTS = 100;

export const useSystemEventsStore = create<SystemEventsState>((set) => ({
  events: [],
  unreadCount: 0,
  addEvent: (event) =>
    set((s) => {
      const next = [event, ...s.events].slice(0, MAX_SYSTEM_EVENTS);
      return { events: next, unreadCount: s.unreadCount + 1 };
    }),
  markAllRead: () =>
    set((s) => ({
      events: s.events.map((e) => ({ ...e, read: true })),
      unreadCount: 0,
    })),
  clearAll: () => set({ events: [], unreadCount: 0 }),
}));

interface ReminderAlertsState {
  alerts: ReminderAlertItem[];
  pushAlert: (alert: ReminderAlertItem) => void;
  dismissAlert: (id: string) => void;
  clearAlerts: () => void;
}

export const useReminderAlertsStore = create<ReminderAlertsState>((set) => ({
  alerts: [],
  pushAlert: (alert) =>
    set((state) => {
      const deduped = state.alerts.filter(
        (item) => item.agentId !== alert.agentId || item.sessionId !== alert.sessionId || item.content !== alert.content
      );
      return { alerts: [alert, ...deduped].slice(0, 5) };
    }),
  dismissAlert: (id) =>
    set((state) => ({ alerts: state.alerts.filter((item) => item.id !== id) })),
  clearAlerts: () => set({ alerts: [] }),
}));

// ── Connection Store ──
interface ConnectionStoreState {
  connected: boolean;
  reason: DisconnectReason;
  reconnectAttempt: number;
  nextRetryMs: number;
  update: (state: ConnectionState) => void;
}

export const useConnectionStore = create<ConnectionStoreState>((set) => ({
  connected: false,
  reason: 'none',
  reconnectAttempt: 0,
  nextRetryMs: 1000,
  update: (state) => set({
    connected: state.connected,
    reason: state.reason,
    reconnectAttempt: state.reconnectAttempt,
    nextRetryMs: state.nextRetryMs,
  }),
}));

// ── Theme Store ──
interface ThemeState {
  theme: 'light' | 'dark' | 'system';
  setTheme: (theme: 'light' | 'dark' | 'system') => void;
}

function resolveInitialTheme(): 'light' | 'dark' | 'system' {
  const saved = localStorage.getItem('blockcell_theme') as 'light' | 'dark' | 'system' | null;
  return saved || 'dark';
}

function applyThemeClass(theme: 'light' | 'dark' | 'system') {
  const isDark =
    theme === 'dark' || (theme === 'system' && window.matchMedia('(prefers-color-scheme: dark)').matches);
  document.documentElement.classList.toggle('dark', isDark);
}

// Apply theme class synchronously on module load so the first render is correct
const _initialTheme = resolveInitialTheme();
applyThemeClass(_initialTheme);

export const useThemeStore = create<ThemeState>((set) => ({
  theme: _initialTheme,
  setTheme: (theme) => {
    set({ theme });
    localStorage.setItem('blockcell_theme', theme);
    applyThemeClass(theme);
  },
}));

export interface AgentOption {
  id: string;
  name: string;
}

interface AgentState {
  selectedAgentId: string;
  agents: AgentOption[];
  setSelectedAgent: (id: string) => void;
  setAgents: (agents: AgentOption[]) => void;
}

function resolveInitialAgentId(): string {
  if (typeof window === 'undefined') return 'default';
  return localStorage.getItem('blockcell_selected_agent') || 'default';
}

export const useAgentStore = create<AgentState>((set) => ({
  selectedAgentId: resolveInitialAgentId(),
  agents: [{ id: 'default', name: 'default' }],
  setSelectedAgent: (id) => {
    localStorage.setItem('blockcell_selected_agent', id);
    set({ selectedAgentId: id });
  },
  setAgents: (agents) => set({ agents }),
}));

// ── Sidebar Store ──
interface SidebarState {
  isOpen: boolean;
  activePage: string;
  toggle: () => void;
  setOpen: (v: boolean) => void;
  setActivePage: (page: string) => void;
}

function getPageFromHash(): string | null {
  if (typeof window === 'undefined') return null;
  const raw = window.location.hash || '';
  const h = raw.startsWith('#') ? raw.slice(1) : raw;
  const page = h.startsWith('/') ? h.slice(1) : h;
  return page || null;
}

function setHashFromPage(page: string) {
  if (typeof window === 'undefined') return;
  const next = `#/${page}`;
  if (window.location.hash !== next) {
    window.location.hash = next;
  }
}

function resolveInitialActivePage(): string {
  const fromHash = getPageFromHash();
  if (fromHash) return fromHash;
  const saved = localStorage.getItem('blockcell_active_page');
  return saved || 'chat';
}

export const useSidebarStore = create<SidebarState>((set) => ({
  isOpen: true,
  activePage: resolveInitialActivePage(),
  toggle: () => set((s) => ({ isOpen: !s.isOpen })),
  setOpen: (v: boolean) => set({ isOpen: v }),
  setActivePage: (page: string) => {
    set({ activePage: page });
    localStorage.setItem('blockcell_active_page', page);
    setHashFromPage(page);
  },
}));

// Keep URL and store in sync on initial load and during back/forward navigation.
if (typeof window !== 'undefined') {
  const w = window as any;
  if (!w.__blockcell_hash_listener_installed) {
    w.__blockcell_hash_listener_installed = true;

    const initial = getPageFromHash();
    if (!initial) {
      setHashFromPage(resolveInitialActivePage());
    }

    window.addEventListener('hashchange', () => {
      const page = getPageFromHash();
      if (!page) return;
      const state = useSidebarStore.getState();
      if (state.activePage !== page) {
        useSidebarStore.setState({ activePage: page });
        localStorage.setItem('blockcell_active_page', page);
      }
    });
  }
}
