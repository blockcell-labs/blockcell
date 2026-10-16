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
    chat_id: 'default:1777350000000',
    client_chat_id: '',
    agent_id: 'default',
  });

  const { sessions, currentSessionId } = useChatStore.getState();
  assert.equal(currentSessionId, 'default_1777350000000');
  assert.equal(sessions.length, 1);
  assert.equal(sessions[0].id, 'default_1777350000000');
  assert.equal(sessions[0].name, '查看深圳明天天气');
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
