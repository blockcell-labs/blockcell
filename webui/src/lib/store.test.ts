import test from 'node:test';
import assert from 'node:assert/strict';

type StorageMap = Map<string, string>;

function createLocalStorage(storage: StorageMap) {
  return {
    getItem(key: string) {
      return storage.has(key) ? storage.get(key)! : null;
    },
    setItem(key: string, value: string) {
      storage.set(key, String(value));
    },
    removeItem(key: string) {
      storage.delete(key);
    },
    clear() {
      storage.clear();
    },
  };
}

const storage = new Map<string, string>();
const localStorageMock = createLocalStorage(storage);

class NotificationMock {
  static permission: NotificationPermission = 'denied';

  static async requestPermission(): Promise<NotificationPermission> {
    return 'denied';
  }

  onclick: (() => void) | null = null;

  constructor(_title: string, _options?: NotificationOptions) {}

  close() {}
}

Object.assign(globalThis, {
  localStorage: localStorageMock,
  Notification: NotificationMock,
  document: {
    hasFocus: () => true,
    documentElement: {
      classList: {
        toggle() {},
      },
    },
  },
});

Object.assign(globalThis, {
  window: {
    BLOCKCELL_API_BASE: 'http://localhost:18790',
    BLOCKCELL_WS_URL: 'ws://localhost:18790/v1/ws',
    localStorage: localStorageMock,
    location: { reload() {}, hash: '' },
    focus() {},
    matchMedia() {
      return { matches: false, addEventListener() {}, removeEventListener() {} };
    },
    addEventListener() {},
    Notification: NotificationMock,
  },
});

const { useChatStore } = await import('./store.js');

function resetStore(currentSessionId = 'chat-1') {
  storage.clear();
  localStorageMock.setItem('blockcell_selected_agent', 'default');
  useChatStore.setState({
    sessions: [],
    currentSessionId,
    currentSessionScope: undefined,
    messages: [],
    isConnected: false,
    isLoading: false,
    pendingFocusSessionId: undefined,
    pendingFocusText: undefined,
  });
}

test('message_done does not duplicate a completed streaming assistant reply', () => {
  resetStore();
  const store = useChatStore.getState();
  const content = '✅ 已设置30秒后的睡觉提醒！';

  store.handleWsEvent({ type: 'token', chat_id: 'chat-1', delta: content });
  store.handleWsEvent({ type: 'message_done', chat_id: 'chat-1' });
  store.handleWsEvent({ type: 'message_done', chat_id: 'chat-1', content });

  const { messages } = useChatStore.getState();
  assert.equal(messages.length, 1);
  assert.equal(messages[0].content, content);
  assert.equal(messages[0].streaming, false);
});

test('stream_reset drops the current streaming assistant message', () => {
  resetStore();
  const store = useChatStore.getState();

  store.handleWsEvent({ type: 'token', chat_id: 'chat-1', delta: 'partial reply' });
  store.handleWsEvent({ type: 'stream_reset', chat_id: 'chat-1' });

  const { messages } = useChatStore.getState();
  assert.equal(messages.length, 0);
});

test('literal underscores do not receive colon session tokens', () => {
  resetStore('a_b');
  useChatStore.getState().handleWsEvent({ type: 'token', agent_id: 'default', chat_id: 'a:b', delta: 'foreign' });
  assert.equal(useChatStore.getState().messages.length, 0);
});

test('old subscriptions cannot reset or error the current session', () => {
  resetStore('chat-B');
  const store = useChatStore.getState();
  store.handleWsEvent({ type: 'token', agent_id: 'default', chat_id: 'chat-B', delta: 'keep' });
  store.setLoading(true);
  for (const agent_id of ['default', 'ops']) {
    store.handleWsEvent({ type: 'stream_reset', agent_id, chat_id: 'chat-A' });
    store.handleWsEvent({ type: 'error', agent_id, chat_id: 'chat-A', message: 'foreign error' });
  }
  assert.equal(useChatStore.getState().messages.length, 1);
  assert.equal(useChatStore.getState().messages[0].content, 'keep');
  assert.equal(useChatStore.getState().isLoading, true);
});

test('unscoped connection errors do not change a chat', () => {
  resetStore('chat-B');
  useChatStore.getState().setLoading(true);
  useChatStore.getState().handleWsEvent({ type: 'error', message: 'connection failure' });
  assert.equal(useChatStore.getState().messages.length, 0);
  assert.equal(useChatStore.getState().isLoading, true);
});

test('message_done still appends a new assistant reply when content changes', () => {
  resetStore();
  const store = useChatStore.getState();
  const preamble = '我来为您设置一个30秒后的睡觉提醒。';
  const finalReply = '✅ 已设置30秒后的睡觉提醒！';

  store.handleWsEvent({ type: 'token', chat_id: 'chat-1', delta: preamble });
  store.handleWsEvent({ type: 'message_done', chat_id: 'chat-1' });
  store.handleWsEvent({ type: 'message_done', chat_id: 'chat-1', content: finalReply });

  const { messages } = useChatStore.getState();
  assert.equal(messages.length, 2);
  assert.equal(messages[0].content, preamble);
  assert.equal(messages[1].content, finalReply);
});

test('message_done does not keep pseudo tool-call text as a visible assistant message', () => {
  resetStore();
  const store = useChatStore.getState();
  const toolTrace = '我来帮您查看上一级目录的内容。<tool_call><function=list_dir><parameter=path>/Users/apple/.blockcell</parameter></function></tool_call>';
  const finalReply = '上一级目录包含 3 个文件夹和 2 个文件。';

  store.handleWsEvent({ type: 'token', chat_id: 'chat-1', delta: toolTrace });
  store.handleWsEvent({ type: 'message_done', chat_id: 'chat-1' });
  store.handleWsEvent({ type: 'message_done', chat_id: 'chat-1', content: finalReply });

  const { messages } = useChatStore.getState();
  assert.equal(messages.length, 1);
  assert.equal(messages[0].content, finalReply);
  assert.equal(messages[0].streaming, false);
});

test('tool_call_start with the same call_id does not create duplicate tool cards', () => {
  resetStore();
  const store = useChatStore.getState();

  store.handleWsEvent({
    type: 'tool_call_start',
    chat_id: 'chat-1',
    call_id: 'call_1',
    tool: 'list_dir',
    params: {},
  });
  store.handleWsEvent({
    type: 'tool_call_start',
    chat_id: 'chat-1',
    call_id: 'call_1',
    tool: 'list_dir',
    params: { path: '/Users/apple/.blockcell/workspace' },
  });

  const { messages } = useChatStore.getState();
  assert.equal(messages.length, 1);
  assert.equal(messages[0].toolCalls?.length, 1);
  assert.deepEqual(messages[0].toolCalls?.[0].params, {
    path: '/Users/apple/.blockcell/workspace',
  });
});

test('session_bound names a new session from the first user message before refresh', () => {
  resetStore('');
  useChatStore.setState({
    messages: [
      {
        id: 'user_1',
        role: 'user',
        content: '查看深圳明天天气',
        timestamp: Date.now(),
      },
    ],
  });

  useChatStore.getState().handleWsEvent({
    type: 'session_bound',
    session_id: 'sid~ws_default_1777350000000',
    chat_id: 'default:1777350000000',
    client_chat_id: '',
    agent_id: 'default',
  });

  const { sessions, currentSessionId } = useChatStore.getState();
  assert.equal(currentSessionId, 'sid~ws_default_1777350000000');
  assert.equal(sessions.length, 1);
  assert.equal(sessions[0].id, 'sid~ws_default_1777350000000');
  assert.equal(sessions[0].name, '查看深圳明天天气');
  assert.equal(useChatStore.getState().getCurrentChatId(), 'default:1777350000000');
});

test('session handles send and cancel using the original chat ID without conversion', async () => {
  const { wsManager } = await import('./ws.js');
  const transport = wsManager as unknown as { ws: { readyState: number; send: (value: string) => void } | null };
  const previousSocket = transport.ws;
  const previousWebSocket = globalThis.WebSocket;
  Object.assign(globalThis, { WebSocket: { OPEN: 1 } });
  const sent: Array<{ type: string; chat_id: string }> = [];
  transport.ws = { readyState: 1, send: (value) => sent.push(JSON.parse(value)) };
  try {
    for (const [id, chat_id] of [['sid~ws_a_b', 'a:b'], ['sid~s~ws~3Aa_b', 'a_b'], ['sid~ws_default_123', 'default:123']]) {
      resetStore(id);
      useChatStore.getState().setSessions([{ id, chat_id, channel: 'ws', agent_id: 'default', name: chat_id, message_count: 0, updated_at: '' }]);
      const originalChatId = useChatStore.getState().getCurrentChatId();
      assert.equal(originalChatId, chat_id);
      assert.equal(wsManager.sendChat('hello', originalChatId, [], 'default'), true);
      assert.equal(wsManager.sendCancel(originalChatId, 'default'), true);
      assert.deepEqual(sent.splice(0).map((message) => message.chat_id), [chat_id, chat_id]);
      useChatStore.getState().handleWsEvent({ type: 'token', agent_id: 'default', chat_id, delta: 'own' });
      assert.equal(useChatStore.getState().messages[0].content, 'own');
    }
  } finally {
    transport.ws = previousSocket;
    Object.assign(globalThis, { WebSocket: previousWebSocket });
  }
});

test('a delayed binding cannot switch an unrelated active session', () => {
  resetStore('a_b');
  useChatStore.getState().addMessage({ id: 'user_other', role: 'user', content: 'unrelated title', timestamp: 1 });
  useChatStore.getState().handleWsEvent({ type: 'session_bound', session_id: 'sid~ws_a_b', agent_id: 'default', chat_id: 'a:b', client_chat_id: '' });
  const state = useChatStore.getState();
  assert.equal(state.currentSessionId, 'a_b');
  assert.equal(state.messages[0].content, 'unrelated title');
  assert.notEqual(state.sessions[0].name, 'unrelated title');
});

test('a list refresh before first persistence preserves the bound conversation scope', () => {
  resetStore('');
  const store = useChatStore.getState();
  store.handleWsEvent({ type: 'session_bound', session_id: 'sid~s~ws~3Aa_b', channel: 'ws', agent_id: 'default', chat_id: 'a_b', client_chat_id: '' });
  useChatStore.getState().setSessions([]);
  assert.equal(useChatStore.getState().getCurrentChatId(), 'a_b');
  useChatStore.getState().handleWsEvent({ type: 'token', channel: 'ws', agent_id: 'default', chat_id: 'a_b', delta: 'first reply' });
  assert.equal(useChatStore.getState().messages[0]?.content, 'first reply');
});

test('a delayed WebSocket binding cannot switch the same chat ID in another channel or account', () => {
  for (const scope of [{ id: 'sid~s~telegram~3Aa_b', channel: 'telegram', account_id: undefined }, { id: 'sid~s~ws~3Aaccount~3A4~3Aworka_b', channel: 'ws', account_id: 'work' }]) {
    resetStore(scope.id);
    useChatStore.getState().setSessions([{ ...scope, chat_id: 'a_b', agent_id: 'default', name: 'other scope', message_count: 0, updated_at: '' }]);
    useChatStore.getState().handleWsEvent({ type: 'session_bound', session_id: 'sid~s~ws~3Aa_b', channel: 'ws', agent_id: 'default', chat_id: 'a_b', client_chat_id: 'a_b' });
    assert.equal(useChatStore.getState().currentSessionId, scope.id);
  }
});

test('background reminders keep server-provided scope for later sending', () => {
  resetStore('sid~ws_other');
  useChatStore.getState().handleWsEvent({ type: 'message_done', channel: 'ws', agent_id: 'default', session_id: 'sid~s~ws~3Aremind_123', chat_id: 'remind_123', content: '⏰ reminder', background_delivery: true, delivery_kind: 'cron' });
  useChatStore.getState().setCurrentSession('sid~s~ws~3Aremind_123');
  assert.equal(useChatStore.getState().getCurrentChatId(), 'remind_123');
});

test('only server-provided unique legacy aliases migrate cached session IDs', () => {
  resetStore('default_123');
  const session = { id: 'sid~ws_default_123', chat_id: 'default:123', legacy_id: 'default_123', channel: 'ws', agent_id: 'default', name: 'default', message_count: 0, updated_at: '' };
  useChatStore.getState().setSessions([session]);
  assert.equal(useChatStore.getState().currentSessionId, session.id);
  assert.equal(useChatStore.getState().getCurrentChatId(), 'default:123');
  resetStore('a_b');
  useChatStore.getState().setSessions([{ ...session, id: 'sid~ws_a_b', legacy_id: 'a_b' }, { ...session, id: 'sid~s~ws~3Aa_b', legacy_id: 'a_b' }]);
  assert.equal(useChatStore.getState().currentSessionId, 'a_b');
  assert.equal(useChatStore.getState().getCurrentChatId(), undefined);
});

test('reset and error require the current channel and Agent, and own errors stop loading', () => {
  resetStore('sid~s~ws~3Aa_b');
  useChatStore.getState().setSessions([{ id: 'sid~s~ws~3Aa_b', chat_id: 'a_b', channel: 'ws', agent_id: 'default', name: 'B', message_count: 0, updated_at: '' }]);
  const store = useChatStore.getState();
  store.handleWsEvent({ type: 'token', channel: 'ws', agent_id: 'default', chat_id: 'a_b', delta: 'keep' });
  store.setLoading(true);
  for (const scope of [{ channel: 'telegram', agent_id: 'default' }, { channel: 'ws', agent_id: 'ops' }]) {
    store.handleWsEvent({ type: 'stream_reset', chat_id: 'a_b', ...scope });
    store.handleWsEvent({ type: 'error', chat_id: 'a_b', message: 'foreign', ...scope });
  }
  assert.equal(useChatStore.getState().messages.length, 1);
  assert.equal(useChatStore.getState().isLoading, true);
  store.handleWsEvent({ type: 'error', channel: 'ws', agent_id: 'default', chat_id: 'a_b', message: 'own failure' });
  assert.equal(useChatStore.getState().messages[1].content, '❌ Error: own failure');
  assert.equal(useChatStore.getState().isLoading, false);
});

// ── Thinking-first streaming tests ──

test('thinking event creates streaming assistant message when no assistant exists', () => {
  resetStore();
  const store = useChatStore.getState();

  store.handleWsEvent({ type: 'thinking', chat_id: 'chat-1', content: '我在思考' });

  const { messages } = useChatStore.getState();
  assert.equal(messages.length, 1);
  assert.equal(messages[0].role, 'assistant');
  assert.equal(messages[0].streaming, true);
  assert.equal(messages[0].reasoning, '我在思考');
  assert.equal(messages[0].content, '');
});

test('token after thinking appends to the same assistant message', () => {
  resetStore();
  const store = useChatStore.getState();

  store.handleWsEvent({ type: 'thinking', chat_id: 'chat-1', content: '我在思考' });
  store.handleWsEvent({ type: 'token', chat_id: 'chat-1', delta: '你好！' });

  const { messages } = useChatStore.getState();
  assert.equal(messages.length, 1);
  assert.equal(messages[0].role, 'assistant');
  assert.equal(messages[0].reasoning, '我在思考');
  assert.equal(messages[0].content, '你好！');
  assert.equal(messages[0].streaming, true);
});

test('message_done preserves reasoning and ends streaming after thinking-first flow', () => {
  resetStore();
  const store = useChatStore.getState();

  store.handleWsEvent({ type: 'thinking', chat_id: 'chat-1', content: '我在思考' });
  store.handleWsEvent({ type: 'token', chat_id: 'chat-1', delta: '你好！' });
  store.handleWsEvent({ type: 'message_done', chat_id: 'chat-1', content: '你好！' });

  const { messages } = useChatStore.getState();
  assert.equal(messages.length, 1);
  assert.equal(messages[0].streaming, false);
  assert.equal(messages[0].reasoning, '我在思考');
  assert.equal(messages[0].content, '你好！');
});

test('tool_call_result marks tool card as error when is_error is true and done otherwise', () => {
  resetStore();
  const store = useChatStore.getState();

  store.handleWsEvent({
    type: 'tool_call_start',
    chat_id: 'chat-1',
    call_id: 'call_ok',
    tool: 'read_file',
    params: { path: 'README.md' },
  });
  store.handleWsEvent({
    type: 'tool_call_start',
    chat_id: 'chat-1',
    call_id: 'call_err',
    tool: 'exec',
    params: { command: 'rm -rf /' },
  });

  store.handleWsEvent({
    type: 'tool_call_result',
    chat_id: 'chat-1',
    call_id: 'call_ok',
    tool: 'read_file',
    result: 'ok content',
    is_error: false,
    outcome: 'succeeded',
    duration_ms: 12,
  });
  store.handleWsEvent({
    type: 'tool_call_result',
    chat_id: 'chat-1',
    call_id: 'call_err',
    tool: 'exec',
    result: { error: 'Policy denied', policy: 'tool_policy' },
    is_error: true,
    outcome: 'rejected',
    duration_ms: 1,
  });

  const { messages } = useChatStore.getState();
  assert.equal(messages.length, 1);
  assert.equal(messages[0].toolCalls?.length, 2);
  assert.equal(messages[0].toolCalls?.[0].status, 'done');
  assert.equal(messages[0].toolCalls?.[1].status, 'error');
});
