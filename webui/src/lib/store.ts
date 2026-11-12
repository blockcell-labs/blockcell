import { create } from 'zustand';
import type { SessionInfo, ChatMsg } from './api';
import type { WsEvent, ConnectionState, DisconnectReason } from './ws';
import { notifyAlertTriggered, notifySystemEvent } from './notifications';

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

function hasNonEmptyToolParams(params: any) {
  if (params == null) return false;
  if (typeof params === 'string') return params.trim().length > 0;
  if (Array.isArray(params)) return params.length > 0;
  if (typeof params === 'object') return Object.keys(params).length > 0;
  return true;
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

function deriveSessionNameFromContent(content?: string) {
  const trimmed = (content || '').trim();
  if (!trimmed) return undefined;

  const name = trimmed.slice(0, 30).trimEnd();
  if (!name) return undefined;
  return trimmed.length > 30 ? `${name}…` : name;
}

function deriveSessionNameFromMessages(messages: UiMessage[]) {
  const firstUserMessage = messages.find((message) => {
    if (message.role !== 'user') return false;
    return !!deriveSessionNameFromContent(message.content);
  });

  return firstUserMessage ? deriveSessionNameFromContent(firstUserMessage.content) : undefined;
}

function isFallbackSessionName(name: string | undefined, sessionIds: string[]) {
  if (!name) return true;
  return sessionIds.some((sessionId) => sessionId && name === sessionId);
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

function promoteSession(state: ChatState, sessionId: string, name?: string, identity?: Partial<SessionInfo>): SessionInfo[] {
  const existing = state.sessions.find((s) => s.id === sessionId);
  const next: SessionInfo = existing
    ? { ...existing, ...identity, name: name || existing.name, updated_at: new Date().toISOString() }
    : {
        id: sessionId,
        name: name || sessionId,
        message_count: 0,
        updated_at: new Date().toISOString(),
        ...identity,
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
  currentSessionScope?: SessionInfo;
  messages: UiMessage[];
  isConnected: boolean;
  isLoading: boolean;
  pendingFocusSessionId?: string;
  pendingFocusText?: string;

  setSessions: (sessions: SessionInfo[]) => void;
  setCurrentSession: (id: string) => void;
  getCurrentChatId: () => string | undefined;
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

function selectedAgentId(): string {
  return typeof window === 'undefined' ? 'default' : (localStorage.getItem('blockcell_selected_agent') || 'default');
}

function currentEventChatId(state: ChatState): string | undefined {
  return currentSessionScope(state)?.chat_id
    ?? (state.currentSessionId.startsWith('sid~') ? undefined : state.currentSessionId);
}

function currentSessionScope(state: ChatState): SessionInfo | undefined {
  return state.sessions.find((session) => session.id === state.currentSessionId)
    ?? (state.currentSessionScope?.id === state.currentSessionId ? state.currentSessionScope : undefined);
}

function eventMatchesCurrentSession(state: ChatState, event: WsEvent, agentId: string): boolean {
  const session = currentSessionScope(state);
  return !!event.chat_id
    && event.chat_id === currentEventChatId(state)
    && (event.agent_id || 'default') === agentId
    && (!session?.agent_id || session.agent_id === agentId)
    && (event.channel || 'ws') === (session?.channel || 'ws')
    && event.account_id === session?.account_id;
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

  setSessions: (sessions) => set((state) => {
    const current = sessions.find((session) => session.id === state.currentSessionId);
    if (!state.currentSessionId || current) {
      return { sessions, currentSessionScope: current };
    }
    const compatible = sessions.filter((session) => session.legacy_id === state.currentSessionId);
    if (compatible.length !== 1) return { sessions };
    const id = compatible[0].id;
    if (typeof window !== 'undefined') localStorage.setItem(sessionStorageKey(selectedAgentId()), id);
    return { sessions, currentSessionId: id, currentSessionScope: compatible[0] };
  }),
  setCurrentSession: (id) => {
    if (typeof window !== 'undefined') {
      const agentId = localStorage.getItem('blockcell_selected_agent') || 'default';
      if (id) {
        localStorage.setItem(sessionStorageKey(agentId), id);
      } else {
        localStorage.removeItem(sessionStorageKey(agentId));
      }
    }
    set({ currentSessionId: id, currentSessionScope: get().sessions.find((session) => session.id === id), messages: [], isLoading: false });
  },
  getCurrentChatId: () => {
    const state = get();
    const session = currentSessionScope(state);
    if (!session || session.channel !== 'ws' || session.account_id
        || (session.agent_id && session.agent_id !== selectedAgentId())) return undefined;
    return session.chat_id;
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
    const agentId = selectedAgentId();

    if (event.type === 'message_done' && event.chat_id) {
      if ((event.agent_id || 'default') !== agentId || (event.channel || 'ws') !== 'ws') {
        return;
      }

      if (!eventMatchesCurrentSession(state, event, agentId)) {
        if ((isCronBackgroundDelivery(event) || looksLikeBackgroundDeliveryMessage(event.content)) && isValidReminderSessionId(event.chat_id)) {
          const sessionId = event.session_id || state.sessions.find((session) =>
            session.chat_id === event.chat_id && session.channel === 'ws'
            && (!session.agent_id || session.agent_id === agentId)
            && session.account_id === event.account_id)?.id;
          if (!sessionId) return;
          set((innerState) => ({ sessions: promoteSession(innerState, sessionId, undefined, {
            channel: 'ws', chat_id: event.chat_id, agent_id: agentId, account_id: event.account_id,
          }) }));
          useReminderAlertsStore.getState().pushAlert({
            id: `reminder-${event.chat_id}-${Date.now()}`,
            sessionId,
            agentId,
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
    const chatEventTypes: string[] = ['message_done', 'token', 'tool_call_start', 'tool_call_result', 'thinking', 'stream_reset', 'error'];
    if (event.type === 'error' && !event.chat_id) {
      useSystemEventsStore.getState().addEvent({
        id: nextMsgId(), kind: 'notification', priority: 'Normal',
        title: 'Connection error', body: event.message || '', timestamp: Date.now(), agentId: event.agent_id,
      });
      return;
    }
    if (chatEventTypes.includes(event.type) && !eventMatchesCurrentSession(state, event, agentId)) {
      return;
    }

    switch (event.type) {
      case 'session_bound': {
        if (!event.chat_id || !event.session_id) {
          break;
        }
        if ((event.agent_id || 'default') !== agentId || (event.channel || 'ws') !== 'ws' || event.account_id) {
          break;
        }

        const realId = event.session_id;
        const clientChatId = event.client_chat_id || '';

        set((innerState) => {
          const existingClientSession = clientChatId
            ? innerState.sessions.find((s) => s.chat_id === clientChatId && s.channel === 'ws'
              && !s.account_id && (!s.agent_id || s.agent_id === agentId))
            : undefined;
          const existingRealSession = innerState.sessions.find((s) => s.id === realId);
          const currentScope = currentSessionScope(innerState);
          const shouldSwitchCurrent = currentEventChatId(innerState) === clientChatId
            && (currentScope?.channel || 'ws') === 'ws'
            && !currentScope?.account_id
            && (!currentScope?.agent_id || currentScope.agent_id === agentId);
          const preferredName = (() => {
            const existingName = existingClientSession?.name || existingRealSession?.name;
            if (!isFallbackSessionName(existingName, [clientChatId, realId, event.chat_id!])) {
              return existingName;
            }
            return shouldSwitchCurrent ? deriveSessionNameFromMessages(innerState.messages) : undefined;
          })();
          const sessionsWithoutClient = innerState.sessions.filter((s) => s.id !== existingClientSession?.id && s.id !== realId);

          const promoted = promoteSession(
            { ...innerState, sessions: sessionsWithoutClient },
            realId,
            preferredName,
            { channel: 'ws', chat_id: event.chat_id, agent_id: agentId },
          );
          if (shouldSwitchCurrent && typeof window !== 'undefined') {
            localStorage.setItem(sessionStorageKey(agentId), realId);
          }

          return {
            sessions: promoted,
            currentSessionId: shouldSwitchCurrent ? realId : innerState.currentSessionId,
            currentSessionScope: shouldSwitchCurrent ? promoted[0] : innerState.currentSessionScope,
          };
        });
        break;
      }

      case 'session_renamed': {
        if (event.chat_id && event.name) {
          if ((event.agent_id || 'default') !== agentId || (event.channel || 'ws') !== 'ws') {
            break;
          }
          const sessionId = event.session_id || state.sessions.find((session) =>
            session.chat_id === event.chat_id && session.channel === 'ws'
            && (!session.agent_id || session.agent_id === agentId)
            && session.account_id === event.account_id)?.id;
          if (!sessionId) break;
          set((state) => {
            return {
              sessions: promoteSession(state, sessionId, event.name!),
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
            // message_done 携带 reasoning_content 时更新 reasoning 字段
            reasoning: event.reasoning_content ?? m.reasoning,
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
            // message_done 携带 reasoning_content 时补入 reasoning 字段
            reasoning: event.reasoning_content ?? m.reasoning,
            media: event.media && event.media.length > 0
              ? [...new Set([...(m.media || []), ...event.media])]
              : m.media,
            highlight: m.highlight || highlight,
          }));
        } else {
          // Non-streaming path (outbound bridge, background delivery, etc.)
          state.addMessage({
            id: nextMsgId(),
            role: 'assistant',
            content: event.content || '',
            reasoning: event.reasoning_content || undefined,
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

      case 'stream_reset': {
        set((s) => {
          const lastMsg = s.messages[s.messages.length - 1];
          if (lastMsg?.role === 'assistant' && lastMsg.streaming) {
            return { messages: s.messages.slice(0, -1) };
          }
          return {};
        });
        break;
      }

      case 'thinking': {
        // 直接使用 set() 并在回调中获取最新状态，确保流式追加正确
        set((s) => {
          const delta = event.content || '';
          if (!delta) return {};

          const lastMsg = s.messages[s.messages.length - 1];
          if (lastMsg?.role === 'assistant' && lastMsg.streaming) {
            const msgs = [...s.messages];
            for (let i = msgs.length - 1; i >= 0; i--) {
              if (msgs[i].role === 'assistant') {
                msgs[i] = { ...msgs[i], reasoning: (msgs[i].reasoning || '') + delta };
                break;
              }
            }
            return { messages: msgs };
          }

          // thinking 先于 token 到达时，创建 assistant streaming message
          return {
            messages: [
              ...s.messages,
              {
                id: nextMsgId(),
                role: 'assistant',
                content: '',
                reasoning: delta,
                timestamp: Date.now(),
                streaming: true,
              },
            ],
          };
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
            toolCalls: (m.toolCalls || []).some((tc) => tc.id === toolCall.id)
              ? (m.toolCalls || []).map((tc) =>
                  tc.id === toolCall.id
                    ? {
                        ...tc,
                        tool: toolCall.tool || tc.tool,
                        params: hasNonEmptyToolParams(toolCall.params) ? toolCall.params : tc.params,
                      }
                    : tc
                )
              : [...(m.toolCalls || []), toolCall],
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
        const nextStatus: 'done' | 'error' = event.is_error ? 'error' : 'done';
        state.updateLastAssistantMessage((m) => ({
          ...m,
          toolCalls: (m.toolCalls || []).map((tc) =>
            tc.id === event.call_id
              ? { ...tc, result: event.result, durationMs: event.duration_ms, status: nextStatus }
              : tc
          ),
        }));
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
