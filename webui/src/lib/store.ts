import { create } from 'zustand';
import type { SessionInfo, ChatMsg } from './api';
import type { WsEvent, ConnectionState, DisconnectReason } from './ws';
import { notifyTaskCompleted, notifyAlertTriggered } from './notifications';

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
}

export interface ToolCallInfo {
  id: string;
  tool: string;
  params: any;
  result?: any;
  durationMs?: number;
  status: 'running' | 'done' | 'error';
}

// ── Chat Store ──
interface ChatState {
  sessions: SessionInfo[];
  currentSessionId: string;
  messages: UiMessage[];
  isConnected: boolean;
  isLoading: boolean;

  setSessions: (sessions: SessionInfo[]) => void;
  setCurrentSession: (id: string) => void;
  setMessages: (messages: UiMessage[]) => void;
  addMessage: (msg: UiMessage) => void;
  updateLastAssistantMessage: (fn: (msg: UiMessage) => UiMessage) => void;
  setConnected: (v: boolean) => void;
  setLoading: (v: boolean) => void;

  // WS event handlers
  handleWsEvent: (event: WsEvent) => void;
}

let msgCounter = 0;
function nextMsgId() {
  return `msg_${Date.now()}_${++msgCounter}`;
}

function resolveInitialSessionId(): string {
  if (typeof window === 'undefined') return 'ws_default';
  const saved = localStorage.getItem('blockcell_last_session_id');
  return saved || `ws_${Date.now()}`;
}

export const useChatStore = create<ChatState>((set, get) => ({
  sessions: [],
  currentSessionId: resolveInitialSessionId(),
  messages: [],
  isConnected: false,
  isLoading: false,

  setSessions: (sessions) => set({ sessions }),
  setCurrentSession: (id) => {
    if (typeof window !== 'undefined') {
      localStorage.setItem('blockcell_last_session_id', id);
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

  handleWsEvent: (event) => {
    const state = get();

    switch (event.type) {
      case 'message_done': {
        // Check if there's a streaming assistant message to finalize
        const lastMsg = state.messages[state.messages.length - 1];
        if (lastMsg?.role === 'assistant' && lastMsg.streaming) {
          state.updateLastAssistantMessage((m) => ({
            ...m,
            content: event.content || m.content,
            streaming: false,
            media: event.media && event.media.length > 0
              ? [...new Set([...(m.media || []), ...event.media])]
              : m.media,
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
          });
        }
        set({ isLoading: false });
        break;
      }

      case 'token': {
        const lastMsg = state.messages[state.messages.length - 1];
        if (lastMsg?.role === 'assistant' && lastMsg.streaming) {
          state.updateLastAssistantMessage((m) => ({
            ...m,
            content: m.content + (event.delta || ''),
          }));
        } else {
          state.addMessage({
            id: nextMsgId(),
            role: 'assistant',
            content: event.delta || '',
            timestamp: Date.now(),
            streaming: true,
          });
        }
        break;
      }

      case 'thinking': {
        const lastMsg = state.messages[state.messages.length - 1];
        if (lastMsg?.role === 'assistant' && lastMsg.streaming) {
          state.updateLastAssistantMessage((m) => ({
            ...m,
            reasoning: (m.reasoning || '') + (event.content || ''),
          }));
        }
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
    }
  },
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
