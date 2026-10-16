import { useEffect, useRef, useState, useCallback, lazy, Suspense } from 'react';
import { Bell, X } from 'lucide-react';
import { Sidebar } from './components/sidebar';
import { LoginPage } from './components/login-page';
import { ConnectionOverlay } from './components/connection-overlay';
import { SetupWizard } from './components/setup-wizard';
import { SystemEventsPanel } from './components/system-events-panel';
import { ThemeProvider } from './components/theme-provider';
import { useSidebarStore, useChatStore, useConnectionStore, useReminderAlertsStore, useAgentStore } from './lib/store';
import { wsManager, type WsEvent } from './lib/ws';
import { WsEventBatcher } from './lib/ws-batcher';
import { cn } from './lib/utils';
import { registerShortcuts, handleGlobalKeyDown } from './lib/keyboard';
import { clearAuthToken, getAuthToken } from './lib/auth';

// Pages are lazy-loaded so each becomes its own chunk: only the active page's
// code is fetched, keeping the initial bundle small. Named exports are mapped
// to the default export shape that React.lazy expects.
const ChatPage = lazy(() => import('./components/chat/chat-page').then((m) => ({ default: m.ChatPage })));
const TasksPage = lazy(() => import('./components/tasks/tasks-page').then((m) => ({ default: m.TasksPage })));
const DashboardPage = lazy(() => import('./components/dashboard/dashboard-page').then((m) => ({ default: m.DashboardPage })));
const ConfigPage = lazy(() => import('./components/config/config-page').then((m) => ({ default: m.ConfigPage })));
const MemoryPage = lazy(() => import('./components/memory/memory-page').then((m) => ({ default: m.MemoryPage })));
const CronPage = lazy(() => import('./components/cron/cron-page').then((m) => ({ default: m.CronPage })));
const AlertsPage = lazy(() => import('./components/alerts/alerts-page').then((m) => ({ default: m.AlertsPage })));
const StreamsPage = lazy(() => import('./components/streams/streams-page').then((m) => ({ default: m.StreamsPage })));
const FilesPage = lazy(() => import('./components/files/files-page').then((m) => ({ default: m.FilesPage })));
const EvolutionPage = lazy(() => import('./components/evolution/evolution-page').then((m) => ({ default: m.EvolutionPage })));
const GhostPage = lazy(() => import('./components/ghost/ghost-page').then((m) => ({ default: m.GhostPage })));
const DeliverablesPage = lazy(() => import('./components/deliverables/deliverables-page').then((m) => ({ default: m.DeliverablesPage })));
const PersonaPage = lazy(() => import('./components/persona/persona-page').then((m) => ({ default: m.PersonaPage })));
const LLMPage = lazy(() => import('./components/llm/llm-page').then((m) => ({ default: m.LLMPage })));
const ChannelsPage = lazy(() => import('./components/channels/channels-page').then((m) => ({ default: m.ChannelsPage })));
const SkillsPage = lazy(() => import('./components/skills/skills-page').then((m) => ({ default: m.SkillsPage })));

interface ConfirmDialog {
  requestId: string;
  tool: string;
  paths: string[];
}

export default function App() {
  const activePage = useSidebarStore((s) => s.activePage);
  const isOpen = useSidebarStore((s) => s.isOpen);
  const setActivePage = useSidebarStore((s) => s.setActivePage);
  const setConnected = useChatStore((s) => s.setConnected);
  const handleWsEvent = useChatStore((s) => s.handleWsEvent);
  const setCurrentSession = useChatStore((s) => s.setCurrentSession);
  const setPendingReminderFocus = useChatStore((s) => s.setPendingReminderFocus);
  const setSessions = useChatStore((s) => s.setSessions);
  const chatSessions = useChatStore((s) => s.sessions);
  const reminderAlerts = useReminderAlertsStore((s) => s.alerts);
  const dismissReminderAlert = useReminderAlertsStore((s) => s.dismissAlert);
  const selectedAgentId = useAgentStore((s) => s.selectedAgentId);
  const visibleReminderAlerts = reminderAlerts.filter((alert) => alert.agentId === selectedAgentId);
  const [authenticated, setAuthenticated] = useState(() => !!getAuthToken());
  const [confirmDialog, setConfirmDialog] = useState<ConfirmDialog | null>(null);
  const [showWizard, setShowWizard] = useState(() => {
    return authenticated && !localStorage.getItem('blockcell_wizard_done');
  });

  const handleLogin = useCallback(() => {
    setAuthenticated(true);
    // Reconnect WS with the newly saved token
    wsManager.forceReconnect();
  }, []);

  const updateConnection = useConnectionStore((s) => s.update);
  const updateConnectionRef = useRef(updateConnection);
  updateConnectionRef.current = updateConnection;

  const handleWsEventRef = useRef(handleWsEvent);
  handleWsEventRef.current = handleWsEvent;
  const setConnectedRef = useRef(setConnected);
  setConnectedRef.current = setConnected;

  useEffect(() => {
    if (getAuthToken()) {
      wsManager.connect();
    }
    const wsEventBatcher = new WsEventBatcher<WsEvent>((event) => {
      if (event.type === 'confirm_request' && event.request_id) {
        setConfirmDialog({ requestId: event.request_id, tool: event.tool_name || event.tool || '', paths: event.paths || [] });
      } else {
        handleWsEventRef.current(event);
      }
    });
    const offConnected = wsManager.on('_connected', () => setConnectedRef.current(true));
    const offDisconnected = wsManager.on('_disconnected', () => setConnectedRef.current(false));
    const offAll = wsManager.on('*', (event) => wsEventBatcher.push(event));
    const offConnection = wsManager.onConnectionChange((state) => {
      updateConnectionRef.current(state);

      // Only force re-login when backend explicitly rejects the token.
      if (state.reason === 'auth_failed') {
        clearAuthToken();
        wsManager.disconnect();
        setAuthenticated(false);
      }
    });

    registerShortcuts();
    window.addEventListener('keydown', handleGlobalKeyDown);

    return () => {
      offConnected();
      offDisconnected();
      offAll();
      offConnection();
      wsEventBatcher.dispose();
      wsManager.disconnect();
      window.removeEventListener('keydown', handleGlobalKeyDown);
    };
  }, []);

  const handleConfirm = useCallback((approved: boolean) => {
    if (confirmDialog) {
      wsManager.sendConfirmResponse(confirmDialog.requestId, approved);
      setConfirmDialog(null);
    }
  }, [confirmDialog]);

  const handleOpenReminder = useCallback((alertId: string, sessionId: string, content: string) => {
    if (!chatSessions.some((session) => session.id === sessionId)) {
      setSessions([
        {
          id: sessionId,
          name: sessionId,
          message_count: 1,
          updated_at: new Date().toISOString(),
        },
        ...chatSessions,
      ]);
    }
    setPendingReminderFocus(sessionId, content);
    setCurrentSession(sessionId);
    setActivePage('chat');
    dismissReminderAlert(alertId);
  }, [chatSessions, dismissReminderAlert, setActivePage, setCurrentSession, setPendingReminderFocus, setSessions]);

  if (!authenticated) {
    return (
      <ThemeProvider>
        <LoginPage onLogin={handleLogin} />
      </ThemeProvider>
    );
  }

  return (
    <ThemeProvider>
      <div className="flex h-screen overflow-hidden">
        <Sidebar />
        <main
          className={cn(
            'flex-1 flex flex-col overflow-hidden transition-all duration-200 relative',
            isOpen ? 'ml-64' : 'ml-16'
          )}
        >
          {visibleReminderAlerts.length > 0 && (
            <div className="absolute top-3 left-1/2 -translate-x-1/2 z-40 flex w-full max-w-2xl flex-col gap-3 px-4 pointer-events-none">
              {visibleReminderAlerts.map((alert) => (
                <div
                  key={alert.id}
                  className="pointer-events-auto rounded-xl border border-[hsl(var(--brand-green)/0.28)] bg-card/95 shadow-2xl backdrop-blur-sm"
                >
                  <div className="flex items-start gap-3 p-4">
                    <div className="mt-0.5 rounded-full bg-[hsl(var(--brand-green)/0.12)] p-2 text-[hsl(var(--brand-green))]">
                      <Bell size={16} />
                    </div>
                    <div className="min-w-0 flex-1">
                      <div className="text-sm font-semibold text-foreground">提醒到了</div>
                      <div className="mt-1 text-sm text-muted-foreground whitespace-pre-wrap break-words">
                        {alert.preview}
                      </div>
                    </div>
                    <button
                      onClick={() => dismissReminderAlert(alert.id)}
                      className="rounded-md p-1 text-muted-foreground hover:bg-accent hover:text-foreground transition-colors"
                    >
                      <X size={14} />
                    </button>
                  </div>
                  <div className="flex justify-end gap-2 border-t border-border px-4 py-3">
                    <button
                      onClick={() => dismissReminderAlert(alert.id)}
                      className="px-3 py-1.5 text-sm rounded-lg border border-border hover:bg-accent transition-colors"
                    >
                      忽略
                    </button>
                    <button
                      onClick={() => handleOpenReminder(alert.id, alert.sessionId, alert.content)}
                      className="px-3 py-1.5 text-sm rounded-lg border border-[hsl(var(--brand-green)/0.28)] bg-[hsl(var(--brand-green)/0.10)] text-[hsl(var(--brand-green))] hover:bg-[hsl(var(--brand-green)/0.16)] transition-colors"
                    >
                      查看提醒
                    </button>
                  </div>
                </div>
              ))}
            </div>
          )}
          {/* System events bell — top right corner */}
          <div className="absolute top-3 right-4 z-30">
            <SystemEventsPanel />
          </div>
          <Suspense fallback={<div className="flex items-center justify-center h-full text-sm text-muted-foreground">Loading…</div>}>
            {activePage === 'chat' && <ChatPage />}
            {activePage === 'tasks' && <TasksPage />}
            {activePage === 'dashboard' && <DashboardPage />}
            {activePage === 'evolution' && <EvolutionPage />}
            {activePage === 'config' && <ConfigPage />}
            {activePage === 'memory' && <MemoryPage />}
            {activePage === 'ghost' && <GhostPage />}
            {activePage === 'cron' && <CronPage />}
            {activePage === 'alerts' && <AlertsPage />}
            {activePage === 'streams' && <StreamsPage />}
            {activePage === 'files' && <FilesPage />}
            {activePage === 'deliverables' && <DeliverablesPage />}
            {activePage === 'persona' && <PersonaPage />}
            {activePage === 'llm' && <LLMPage />}
            {activePage === 'channels' && <ChannelsPage />}
            {activePage === 'skills' && <SkillsPage />}
          </Suspense>
        </main>
        <ConnectionOverlay />
        {showWizard && (
          <SetupWizard
            onComplete={() => setShowWizard(false)}
            onSkip={() => {
              localStorage.setItem('blockcell_wizard_done', '1');
              setShowWizard(false);
            }}
          />
        )}
        {/* Path access confirmation dialog */}
        {confirmDialog && (
          <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm">
            <div className="bg-card border border-border rounded-xl shadow-2xl max-w-md w-full mx-4 p-6 space-y-4">
              <div className="flex items-start gap-3">
                <span className="text-2xl">⚠️</span>
                <div>
                  <h2 className="font-semibold text-foreground">安全确认 / Security Confirmation</h2>
                  <p className="text-sm text-muted-foreground mt-1">
                    工具 <code className="font-mono text-[hsl(var(--brand-green))]">{confirmDialog.tool}</code> 请求访问工作区以外的路径：
                  </p>
                </div>
              </div>
              <ul className="space-y-1 max-h-40 overflow-y-auto">
                {confirmDialog.paths.map((p) => (
                  <li key={p} className="text-xs font-mono bg-muted/50 rounded px-3 py-1.5 break-all">
                    📁 {p}
                  </li>
                ))}
              </ul>
              <p className="text-sm text-muted-foreground">是否允许访问？/ Allow access?</p>
              <div className="flex gap-3 justify-end">
                <button
                  onClick={() => handleConfirm(false)}
                  className="px-4 py-2 text-sm rounded-lg border border-border hover:bg-accent transition-colors"
                >
                  拒绝 / Deny
                </button>
                <button
                  onClick={() => handleConfirm(true)}
                  className="px-4 py-2 text-sm rounded-lg border border-[hsl(var(--brand-green)/0.28)] bg-[hsl(var(--brand-green)/0.10)] text-[hsl(var(--brand-green))] hover:bg-[hsl(var(--brand-green)/0.16)] transition-colors"
                >
                  允许 / Allow
                </button>
              </div>
            </div>
          </div>
        )}
      </div>
    </ThemeProvider>
  );
}
