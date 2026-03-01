import { useEffect, useRef, useState, useCallback } from 'react';
import { Sidebar } from './components/sidebar';
import { ChatPage } from './components/chat/chat-page';
import { TasksPage } from './components/tasks/tasks-page';
import { DashboardPage } from './components/dashboard/dashboard-page';
import { ConfigPage } from './components/config/config-page';
import { MemoryPage } from './components/memory/memory-page';
import { CronPage } from './components/cron/cron-page';
import { AlertsPage } from './components/alerts/alerts-page';
import { StreamsPage } from './components/streams/streams-page';
import { FilesPage } from './components/files/files-page';
import { EvolutionPage } from './components/evolution/evolution-page';
import { GhostPage } from './components/ghost/ghost-page';
import { LoginPage } from './components/login-page';
import { ConnectionOverlay } from './components/connection-overlay';
import { ThemeProvider } from './components/theme-provider';
import { useSidebarStore, useChatStore, useConnectionStore } from './lib/store';
import { wsManager } from './lib/ws';
import { cn } from './lib/utils';
import { requestNotificationPermission } from './lib/notifications';
import { registerShortcuts, handleGlobalKeyDown } from './lib/keyboard';

interface ConfirmDialog {
  requestId: string;
  tool: string;
  paths: string[];
}

export default function App() {
  const { activePage, isOpen } = useSidebarStore();
  const { setConnected, handleWsEvent } = useChatStore();
  const [authenticated, setAuthenticated] = useState(() => !!localStorage.getItem('blockcell_token'));
  const [confirmDialog, setConfirmDialog] = useState<ConfirmDialog | null>(null);

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
    if (localStorage.getItem('blockcell_token')) {
      wsManager.connect();
    }
    const offConnected = wsManager.on('_connected', () => setConnectedRef.current(true));
    const offDisconnected = wsManager.on('_disconnected', () => setConnectedRef.current(false));
    const offAll = wsManager.on('*', (event) => {
      if (event.type === 'confirm_request' && event.request_id) {
        setConfirmDialog({ requestId: event.request_id, tool: event.tool || '', paths: event.paths || [] });
      } else {
        handleWsEventRef.current(event);
      }
    });
    const offConnection = wsManager.onConnectionChange((state) => {
      updateConnectionRef.current(state);

      // Only force re-login when backend explicitly rejects the token.
      if (state.reason === 'auth_failed') {
        localStorage.removeItem('blockcell_token');
        wsManager.disconnect();
        setAuthenticated(false);
      }
    });

    requestNotificationPermission();

    registerShortcuts();
    window.addEventListener('keydown', handleGlobalKeyDown);

    return () => {
      offConnected();
      offDisconnected();
      offAll();
      offConnection();
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
            'flex-1 flex flex-col overflow-hidden transition-all duration-200',
            isOpen ? 'ml-64' : 'ml-16'
          )}
        >
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
        </main>
        <ConnectionOverlay />
        {/* Path access confirmation dialog */}
        {confirmDialog && (
          <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm">
            <div className="bg-card border border-border rounded-xl shadow-2xl max-w-md w-full mx-4 p-6 space-y-4">
              <div className="flex items-start gap-3">
                <span className="text-2xl">⚠️</span>
                <div>
                  <h2 className="font-semibold text-foreground">安全确认 / Security Confirmation</h2>
                  <p className="text-sm text-muted-foreground mt-1">
                    工具 <code className="text-cyber font-mono">{confirmDialog.tool}</code> 请求访问工作区以外的路径：
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
                  className="px-4 py-2 text-sm rounded-lg bg-cyber/20 border border-cyber/40 text-cyber hover:bg-cyber/30 transition-colors"
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
