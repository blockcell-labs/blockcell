import { authProtocol, clearAuthToken, getAuthToken } from './auth';
import { forCurrentSocket, sendJson } from './ws-connection';

export type WsEventType =
  | 'token'
  | 'stream_reset'
  | 'tool_call_start'
  | 'tool_call_result'
  | 'message_done'
  | 'session_bound'
  | 'confirm_request'
  | 'error'
  | 'thinking'
  | 'alert_triggered'
  | 'skills_updated'
  | 'evolution_triggered'
  | 'session_renamed'
  | 'system_event_notification'
  | 'system_event_summary'
  | 'agent_progress'
  | 'agent_stage';

export interface WsEvent {
  type: WsEventType;
  agent_id?: string;
  chat_id?: string;
  client_chat_id?: string;
  background_delivery?: boolean;
  delivery_kind?: string;
  cron_kind?: string;
  task_id?: string;
  delta?: string;
  content?: string;
  reasoning_content?: string;
  tool?: string;
  tool_name?: string;
  call_id?: string;
  params?: any;
  result?: any;
  is_error?: boolean;
  outcome?: string;
  duration_ms?: number;
  tool_calls?: number;
  status?: string;
  label?: string;
  request_id?: string;
  paths?: string[];
  message?: string;
  alert_name?: string;
  alert_value?: number;
  new_skills?: string[];
  media?: string[];
  name?: string;
  // system event fields
  event_id?: string;
  priority?: string;
  title?: string;
  body?: string;
  compact_text?: string;
  items?: any[];
  // agent progress fields
  progress_type?: string;
  tokens_added?: number;
  tools_added?: number;
  total_tokens?: number;
  total_tools?: number;
  // agent stage fields
  stage?: string;
  percent?: number;
}

export type DisconnectReason = 'none' | 'auth_failed' | 'network_error' | 'server_down' | 'connecting' | 'reconnect_exhausted';

export interface ConnectionState {
  connected: boolean;
  reason: DisconnectReason;
  reconnectAttempt: number;
  nextRetryMs: number;
}

type WsListener = (event: WsEvent) => void;
type ConnectionListener = (state: ConnectionState) => void;

declare global {
  interface Window {
    BLOCKCELL_API_BASE?: string;
    BLOCKCELL_WS_URL?: string;
  }
}

function resolveApiBase(): string {
  return (typeof window !== 'undefined' && window.BLOCKCELL_API_BASE) || import.meta.env.VITE_API_BASE || 'http://localhost:18790';
}

function resolveWsUrl(): string {
  if (typeof window !== 'undefined' && window.BLOCKCELL_WS_URL) return window.BLOCKCELL_WS_URL;
  if (import.meta.env.VITE_WS_URL) return import.meta.env.VITE_WS_URL;

  const apiBase = resolveApiBase();
  const wsProto = apiBase.startsWith('https://') ? 'wss://' : 'ws://';
  const host = apiBase.replace(/^https?:\/\//, '');
  return `${wsProto}${host}/v1/ws`;
}

class WebSocketManager {
  private ws: WebSocket | null = null;
  private listeners: Map<string, Set<WsListener>> = new Map();
  private connectionListeners: Set<ConnectionListener> = new Set();
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private reconnectDelay = 1000;
  private maxReconnectDelay = 30000;
  private maxReconnectAttempts = 20;
  private url: string;
  private shouldReconnect = true;
  private _reconnectAttempt = 0;
  private _reason: DisconnectReason = 'none';
  private _wasConnected = false;
  private _generation = 0;
  private _healthProbed = false;
  private _emitTimer: ReturnType<typeof setTimeout> | null = null;
  private _emitScheduled = false;

  constructor() {
    this.url = resolveWsUrl();
  }

  connect() {
    if (this.ws?.readyState === WebSocket.OPEN || this.ws?.readyState === WebSocket.CONNECTING) return;

    if (this._reason === 'reconnect_exhausted') return;

    this.shouldReconnect = true;
    this._reason = 'connecting';
    this._generation++;
    this.emitConnectionState();

    try {
      const token = getAuthToken();
      const socket = token
        ? new WebSocket(this.url, [authProtocol(token)])
        : new WebSocket(this.url);
      this.ws = socket;

      socket.onopen = forCurrentSocket(() => this.ws, socket, () => {
        this.reconnectDelay = 1000;
        this._reconnectAttempt = 0;
        this._reason = 'none';
        this._wasConnected = true;
        this.emitInternal('_connected');
        this.emitConnectionState();
      });

      socket.onmessage = forCurrentSocket(() => this.ws, socket, (event: MessageEvent) => {
        try {
          const data: WsEvent = JSON.parse(event.data);
          this.emit(data.type, data);
          this.emit('*', data);
        } catch {
          // ignore non-JSON messages
        }
      });

      socket.onclose = forCurrentSocket(() => this.ws, socket, (event: CloseEvent) => {
        // Only treat explicit 4401 as auth failure.
        // code 1006 (abnormal close) is ambiguous — it fires on server restarts,
        // network blips, and CORS issues, NOT just auth failures. Treating it as
        // auth_failed causes logged-in users to be kicked out on page refresh.
        if (event.code === 4401) {
          this._reason = 'auth_failed';
          this.shouldReconnect = false;
          if (this.reconnectTimer) {
            clearTimeout(this.reconnectTimer);
            this.reconnectTimer = null;
          }
        } else if (event.code === 1006) {
          this._reason = this._wasConnected ? 'network_error' : 'server_down';
        } else {
          this._reason = this._wasConnected ? 'network_error' : 'server_down';
        }

        // If the browser had a stored token, and the WS failed before ever being connected,
        // this is often caused by the gateway restarting and invalidating the token.
        // Probe /v1/health to distinguish auth failure from server down.
        if (this._reason === 'server_down' && !this._healthProbed) {
          const token = getAuthToken();
          if (token && !this._wasConnected) {
            this._healthProbed = true;
            const gen = this._generation;
            void this.probeHealthAndSetReason(gen);
          }
        }

        this._wasConnected = false;
        this.emitInternal('_disconnected');
        this.emitConnectionState();

        if (this.shouldReconnect && this._reason !== 'auth_failed') {
          this.scheduleReconnect();
        }
      });

      socket.onerror = forCurrentSocket(() => this.ws, socket, () => {
        // Only close if still connecting/open — avoids "closed before established" warning
        if (socket.readyState === WebSocket.OPEN || socket.readyState === WebSocket.CONNECTING) {
          socket.close();
        }
      });
    } catch {
      this._reason = 'network_error';
      this.emitConnectionState();
      this.scheduleReconnect();
    }
  }

  disconnect() {
    this.shouldReconnect = false;
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    this.ws?.close();
    this.ws = null;
    this._reason = 'none';
    this._reconnectAttempt = 0;
    this.emitConnectionState();
  }

  /** Force reconnect — used by the overlay's "Retry" button */
  forceReconnect() {
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    this.shouldReconnect = true;
    this.ws?.close();
    this.ws = null;
    this.reconnectDelay = 1000;
    this._reconnectAttempt = 0;
    this._reason = 'connecting';
    this._wasConnected = false;
    this._healthProbed = false;
    this.emitConnectionState();
    this.connect();
  }

  /** Re-login: clear token and signal auth_failed so App shows login */
  relogin() {
    this.disconnect();
    clearAuthToken();
    window.location.reload();
  }

  private async probeHealthAndSetReason(gen: number) {
    try {
      const res = await fetch(`${resolveApiBase()}/v1/health`, { signal: AbortSignal.timeout(3000) });
      // If a newer connect() has started, discard this stale probe result
      if (gen !== this._generation) return;
      if (res.ok) {
        // Server is up but WS was rejected → auth failure
        this._reason = 'auth_failed';
        // Stop reconnecting — stale token won't help
        this.shouldReconnect = false;
        if (this.reconnectTimer) {
          clearTimeout(this.reconnectTimer);
          this.reconnectTimer = null;
        }
      } else {
        this._reason = 'server_down';
      }
    } catch {
      if (gen !== this._generation) return;
      this._reason = 'server_down';
    }
    this.emitConnectionState();
  }

  private scheduleReconnect() {
    if (this.reconnectTimer) return;
    if (this._reconnectAttempt >= this.maxReconnectAttempts) {
      this.shouldReconnect = false;
      this._reason = 'reconnect_exhausted';
      this.emitConnectionState();
      return;
    }
    this._reconnectAttempt++;
    const delay = this.reconnectDelay;
    this.emitConnectionState();
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      this.reconnectDelay = Math.min(this.reconnectDelay * 2, this.maxReconnectDelay);
      this.connect();
    }, delay);
  }

  send(data: { type: string; content?: string; chat_id?: string; media?: string[]; agent_id?: string; [key: string]: unknown }): boolean {
    return sendJson(this.ws, data, WebSocket.OPEN);
  }

  sendChat(content: string, chatId?: string, media: string[] = [], agentId?: string): boolean {
    return this.send({ type: 'chat', content, chat_id: chatId, media, agent_id: agentId });
  }

  sendCancel(chatId = 'default', agentId?: string): boolean {
    return this.send({ type: 'cancel', chat_id: chatId, agent_id: agentId });
  }

  sendConfirmResponse(requestId: string, approved: boolean): boolean {
    return this.send({ type: 'confirm_response', request_id: requestId, approved });
  }

  on(event: string, listener: WsListener) {
    if (!this.listeners.has(event)) {
      this.listeners.set(event, new Set());
    }
    this.listeners.get(event)!.add(listener);
    return () => this.off(event, listener);
  }

  off(event: string, listener: WsListener) {
    this.listeners.get(event)?.delete(listener);
  }

  onConnectionChange(listener: ConnectionListener) {
    this.connectionListeners.add(listener);
    return () => this.connectionListeners.delete(listener);
  }

  private emit(event: string, data: WsEvent) {
    this.listeners.get(event)?.forEach((fn) => fn(data));
  }

  /** Emit internal events (like _connected/_disconnected) without triggering '*' wildcard */
  private emitInternal(event: string) {
    this.listeners.get(event)?.forEach((fn) => fn({ type: 'token' } as WsEvent));
  }

  private emitConnectionState() {
    if (this._emitScheduled) return;
    this._emitScheduled = true;
    if (this._emitTimer) clearTimeout(this._emitTimer);
    this._emitTimer = setTimeout(() => {
      this._emitTimer = null;
      this._emitScheduled = false;
      const state = this.connectionState;
      this.connectionListeners.forEach((fn) => fn(state));
    }, 100);
  }

  get connected() {
    return this.ws?.readyState === WebSocket.OPEN;
  }

  get connectionState(): ConnectionState {
    return {
      connected: this.ws?.readyState === WebSocket.OPEN,
      reason: this._reason,
      reconnectAttempt: this._reconnectAttempt,
      nextRetryMs: this.reconnectDelay,
    };
  }
}

export const wsManager = new WebSocketManager();
