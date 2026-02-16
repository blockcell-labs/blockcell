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

export default function App() {
  const { activePage, isOpen } = useSidebarStore();
  const { setConnected, handleWsEvent } = useChatStore();
  const [authenticated, setAuthenticated] = useState(() => !!localStorage.getItem('blockcell_token'));

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
    const offAll = wsManager.on('*', (event) => handleWsEventRef.current(event));
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
      </div>
    </ThemeProvider>
  );
}
