export type WsEventType =
  | 'token'
  | 'tool_call_start'
  | 'tool_call_result'
  | 'message_done'
  | 'task_update'
  | 'confirm_request'
  | 'error'
  | 'thinking'
  | 'alert_triggered'
  | 'skills_updated';

export interface WsEvent {
  type: WsEventType;
  chat_id?: string;
  task_id?: string;
  delta?: string;
  content?: string;
  tool?: string;
  call_id?: string;
  params?: any;
  result?: any;
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
}

export type DisconnectReason = 'none' | 'auth_failed' | 'network_error' | 'server_down' | 'connecting';

export interface ConnectionState {
  connected: boolean;
  reason: DisconnectReason;
  reconnectAttempt: number;
  nextRetryMs: number;
}

type WsListener = (event: WsEvent) => void;
type ConnectionListener = (state: ConnectionState) => void;

const API_BASE = import.meta.env.VITE_API_BASE || 'http://localhost:18790';

class WebSocketManager {
  private ws: WebSocket | null = null;
  private listeners: Map<string, Set<WsListener>> = new Map();
  private connectionListeners: Set<ConnectionListener> = new Set();
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private reconnectDelay = 1000;
  private maxReconnectDelay = 30000;
  private url: string;
  private shouldReconnect = true;
  private _reconnectAttempt = 0;
  private _reason: DisconnectReason = 'none';
  private _wasConnected = false;
  private _generation = 0;

  constructor() {
    this.url = import.meta.env.VITE_WS_URL || 'ws://localhost:18790/v1/ws';
  }

  connect() {
    if (this.ws?.readyState === WebSocket.OPEN || this.ws?.readyState === WebSocket.CONNECTING) return;

    this.shouldReconnect = true;
    this._reason = 'connecting';
    this._generation++;
    this.emitConnectionState();

    try {
      const token = localStorage.getItem('blockcell_token');
      const url = token ? `${this.url}?token=${token}` : this.url;
      this.ws = new WebSocket(url);

      this.ws.onopen = () => {
        this.reconnectDelay = 1000;
        this._reconnectAttempt = 0;
        this._reason = 'none';
        this._wasConnected = true;
        this.emitInternal('_connected');
        this.emitConnectionState();
      };

      this.ws.onmessage = (event) => {
        try {
          const data: WsEvent = JSON.parse(event.data);
          this.emit(data.type, data);
          this.emit('*', data);
        } catch {
          // ignore non-JSON messages
        }
      };

      this.ws.onclose = (event) => {
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

        this._wasConnected = false;
        this.emitInternal('_disconnected');
        this.emitConnectionState();

        if (this.shouldReconnect && this._reason !== 'auth_failed') {
          this.scheduleReconnect();
        }
      };

      this.ws.onerror = () => {
        // Only close if still connecting/open — avoids "closed before established" warning
        if (this.ws && (this.ws.readyState === WebSocket.OPEN || this.ws.readyState === WebSocket.CONNECTING)) {
          this.ws.close();
        }
      };
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
    this.ws?.close();
    this.ws = null;
    this.reconnectDelay = 1000;
    this._reconnectAttempt = 0;
    this._reason = 'connecting';
    this._wasConnected = false;
    this.emitConnectionState();
    this.connect();
  }

  /** Re-login: clear token and signal auth_failed so App shows login */
  relogin() {
    this.disconnect();
    localStorage.removeItem('blockcell_token');
    window.location.reload();
  }

  private async probeHealthAndSetReason(gen: number) {
    try {
      const res = await fetch(`${API_BASE}/v1/health`, { signal: AbortSignal.timeout(3000) });
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
    this._reconnectAttempt++;
    const delay = this.reconnectDelay;
    this.emitConnectionState();
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      this.reconnectDelay = Math.min(this.reconnectDelay * 2, this.maxReconnectDelay);
      this.connect();
    }, delay);
  }

  send(data: { type: string; content?: string; chat_id?: string; media?: string[] }) {
    if (this.ws?.readyState === WebSocket.OPEN) {
      this.ws.send(JSON.stringify(data));
    }
  }

  sendChat(content: string, chatId = 'default', media: string[] = []) {
    this.send({ type: 'chat', content, chat_id: chatId, media });
  }

  sendConfirmResponse(requestId: string, approved: boolean) {
    this.send({ type: 'confirm_response', content: JSON.stringify({ request_id: requestId, approved }) });
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
    const state = this.connectionState;
    this.connectionListeners.forEach((fn) => fn(state));
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
