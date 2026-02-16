import { useEffect, useRef, useState } from 'react';
import {
  MessageSquare, ListTodo, LayoutDashboard, Settings, Brain,
  Clock, ChevronLeft, ChevronRight, Plus, Trash2, Sun, Moon,
  Wifi, WifiOff, Bell, Radio, FolderOpen, AlertTriangle, LogOut, Dna, Ghost,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import { useSidebarStore, useChatStore, useThemeStore } from '@/lib/store';
import { getSessionsPage, deleteSession, logout, type SessionInfo } from '@/lib/api';
import { useT } from '@/lib/i18n';
import { BlockcellLogo } from './blockcell-logo';

const navItems = [
  { id: 'chat', key: 'nav.chat', icon: MessageSquare },
  { id: 'tasks', key: 'nav.tasks', icon: ListTodo },
  { id: 'dashboard', key: 'nav.dashboard', icon: LayoutDashboard },
  { id: 'evolution', key: 'nav.evolution', icon: Dna },
  { id: 'memory', key: 'nav.memory', icon: Brain },
  { id: 'ghost', key: 'nav.ghost', icon: Ghost },
  { id: 'cron', key: 'nav.cron', icon: Clock },
  { id: 'alerts', key: 'nav.alerts', icon: Bell },
  { id: 'streams', key: 'nav.streams', icon: Radio },
  { id: 'files', key: 'nav.files', icon: FolderOpen },
  { id: 'config', key: 'nav.settings', icon: Settings },
];

export function Sidebar() {
  const { isOpen, activePage, toggle, setActivePage } = useSidebarStore();
  const { sessions, setSessions, currentSessionId, setCurrentSession, isConnected } = useChatStore();
  const { theme, setTheme } = useThemeStore();
  const t = useT();
  const [loadingSessions, setLoadingSessions] = useState(false);
  const [loadingMoreSessions, setLoadingMoreSessions] = useState(false);
  const [nextCursor, setNextCursor] = useState<number | null>(0);
  const sessionsRef = useRef<SessionInfo[]>(sessions);
  const [deleteConfirm, setDeleteConfirm] = useState<{ id: string; name: string } | null>(null);
  const [logoutConfirm, setLogoutConfirm] = useState(false);

  sessionsRef.current = sessions;

  useEffect(() => {
    loadSessions();
  }, []);

  async function loadSessions() {
    setLoadingSessions(true);
    try {
      const data = await getSessionsPage({ limit: 12, cursor: 0 });
      setSessions(data.sessions);
      setNextCursor(data.next_cursor);
    } catch {
      // ignore
    } finally {
      setLoadingSessions(false);
    }
  }

  async function loadMoreSessions() {
    if (loadingMoreSessions) return;
    if (nextCursor === null) return;

    setLoadingMoreSessions(true);
    try {
      const data = await getSessionsPage({ limit: 12, cursor: nextCursor });
      if (data.sessions?.length) {
        setSessions([...sessionsRef.current, ...data.sessions]);
      }
      setNextCursor(data.next_cursor);
    } catch {
      // ignore
    } finally {
      setLoadingMoreSessions(false);
    }
  }

  function onSessionsScroll(e: React.UIEvent<HTMLDivElement>) {
    const el = e.currentTarget;
    if (el.scrollTop + el.clientHeight >= el.scrollHeight - 24) {
      loadMoreSessions();
    }
  }

  function requestDeleteSession(id: string, name: string, e: React.MouseEvent) {
    e.stopPropagation();
    setDeleteConfirm({ id, name });
  }

  async function confirmDeleteSession() {
    if (!deleteConfirm) return;
    const id = deleteConfirm.id;
    try {
      await deleteSession(id);
      setSessions(sessions.filter((s) => s.id !== id));
      if (currentSessionId === id) {
        setCurrentSession(`ws_${Date.now()}`);
      }
    } catch {
      // ignore
    } finally {
      setDeleteConfirm(null);
    }
  }

  function handleNewChat() {
    setCurrentSession(`ws_${Date.now()}`);
    setActivePage('chat');
  }

  return (
    <aside
      className={cn(
        'fixed left-0 top-0 h-full bg-card border-r border-border flex flex-col transition-all duration-200 z-40',
        isOpen ? 'w-64' : 'w-16'
      )}
    >
      {/* Header */}
      <div className="flex items-center justify-between p-3 border-b border-border">
        {isOpen ? (
          <div className="flex items-center gap-2.5">
            <BlockcellLogo size="xs" className="shrink-0" />
            <span className="font-bold text-sm tracking-wider">
              BLOCK<span className="text-cyber">CELL</span>
            </span>
            <span className={cn('w-2 h-2 rounded-full', isConnected ? 'bg-cyber' : 'bg-red-500')} />
          </div>
        ) : (
          <div className="mx-auto shrink-0">
            <BlockcellLogo size="xs" />
          </div>
        )}
        <button onClick={toggle} className="p-1.5 rounded-md hover:bg-accent text-muted-foreground">
          {isOpen ? <ChevronLeft size={16} /> : <ChevronRight size={16} />}
        </button>
      </div>

      {/* Navigation */}
      <nav className="flex-1 overflow-y-auto py-2">
        {navItems.map((item) => (
          <button
            key={item.id}
            onClick={() => setActivePage(item.id)}
            className={cn(
              'w-full flex items-center text-sm transition-colors',
              isOpen ? 'gap-3 px-3 py-2 justify-start' : 'px-0 py-2.5 justify-center',
              activePage === item.id
                ? 'bg-rust/10 text-rust border-r-2 border-rust'
                : 'text-muted-foreground hover:bg-accent/50 hover:text-foreground'
            )}
          >
            <item.icon size={isOpen ? 18 : 22} className="shrink-0" />
            {isOpen && <span>{t(item.key)}</span>}
          </button>
        ))}

        {/* Session list (only when chat is active and sidebar is open) */}
        {isOpen && activePage === 'chat' && (
          <div className="mt-3 px-2">
            <div className="flex items-center justify-between px-1 mb-1">
              <span className="text-xs font-medium text-muted-foreground uppercase">{t('sidebar.sessions')}</span>
              <button
                onClick={handleNewChat}
                className="p-1 rounded hover:bg-accent text-muted-foreground"
                title={t('sidebar.newChat')}
              >
                <Plus size={14} />
              </button>
            </div>
            <div className="space-y-0.5 max-h-[40vh] overflow-y-auto" onScroll={onSessionsScroll}>
              {sessions.map((s) => (
                <div
                  key={s.id}
                  onClick={() => { setCurrentSession(s.id); setActivePage('chat'); }}
                  className={cn(
                    'group flex items-center justify-between px-2 py-1.5 rounded-md text-xs cursor-pointer transition-colors',
                    currentSessionId === s.id
                      ? 'bg-accent text-accent-foreground'
                      : 'text-muted-foreground hover:bg-accent/50'
                  )}
                >
                  <span className="truncate flex-1">{s.name}</span>
                  <button
                    onClick={(e) => requestDeleteSession(s.id, s.name, e)}
                    className="opacity-0 group-hover:opacity-100 p-0.5 rounded hover:bg-destructive/20 text-destructive"
                  >
                    <Trash2 size={12} />
                  </button>
                </div>
              ))}

              {loadingMoreSessions && (
                <div className="px-2 py-2 text-[11px] text-muted-foreground">
                  {t('common.loading')}
                </div>
              )}

              {!loadingMoreSessions && nextCursor !== null && sessions.length > 0 && (
                <div className="px-2 py-2 text-[11px] text-muted-foreground">
                  {t('sidebar.scrollToLoadMore')}
                </div>
              )}
            </div>
          </div>
        )}
      </nav>

      {/* Footer */}
      <div className="border-t border-border p-2 flex items-center justify-between">
        {isOpen ? (
          <>
            <div className="flex items-center gap-1 text-xs text-muted-foreground">
              {isConnected ? <Wifi size={12} className="text-cyber" /> : <WifiOff size={12} className="text-red-500" />}
              <span>{isConnected ? t('sidebar.connected') : t('sidebar.disconnected')}</span>
            </div>
            <div className="flex items-center gap-0.5">
              <button
                onClick={() => setTheme(theme === 'dark' ? 'light' : 'dark')}
                className="p-1.5 rounded-md hover:bg-accent text-muted-foreground"
                title={theme === 'dark' ? t('sidebar.lightMode') : t('sidebar.darkMode')}
              >
                {theme === 'dark' ? <Sun size={14} /> : <Moon size={14} />}
              </button>
              <button
                onClick={() => setLogoutConfirm(true)}
                className="p-1.5 rounded-md hover:bg-destructive/20 text-muted-foreground"
                title={t('sidebar.logout')}
              >
                <LogOut size={14} />
              </button>
            </div>
          </>
        ) : (
          <div className="flex flex-col items-center gap-1 mx-auto">
            <button
              onClick={() => setTheme(theme === 'dark' ? 'light' : 'dark')}
              className="p-1.5 rounded-md hover:bg-accent text-muted-foreground"
              title={theme === 'dark' ? t('sidebar.lightMode') : t('sidebar.darkMode')}
            >
              {theme === 'dark' ? <Sun size={14} /> : <Moon size={14} />}
            </button>
            <button
              onClick={() => setLogoutConfirm(true)}
              className="p-1.5 rounded-md hover:bg-destructive/20 text-muted-foreground"
              title={t('sidebar.logout')}
            >
              <LogOut size={14} />
            </button>
          </div>
        )}
      </div>
      {/* Logout confirmation dialog */}
      {logoutConfirm && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50" onClick={() => setLogoutConfirm(false)}>
          <div className="bg-card border border-border rounded-xl p-6 max-w-sm w-full mx-4 shadow-xl" onClick={(e) => e.stopPropagation()}>
            <div className="flex items-center gap-3 mb-4">
              <div className="p-2 rounded-full bg-destructive/10">
                <LogOut size={20} className="text-destructive" />
              </div>
              <h3 className="font-semibold">{t('sidebar.logout')}</h3>
            </div>
            <p className="text-sm text-muted-foreground mb-6">{t('sidebar.logoutConfirm')}</p>
            <div className="flex justify-end gap-2">
              <button
                onClick={() => setLogoutConfirm(false)}
                className="px-4 py-1.5 text-sm rounded-lg border border-border hover:bg-accent"
              >
                {t('common.cancel')}
              </button>
              <button
                onClick={logout}
                className="px-4 py-1.5 text-sm rounded-lg bg-destructive text-destructive-foreground hover:bg-destructive/90"
              >
                {t('sidebar.logout')}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Delete session confirmation dialog */}
      {deleteConfirm && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50" onClick={() => setDeleteConfirm(null)}>
          <div className="bg-card border border-border rounded-xl p-6 max-w-sm w-full mx-4 shadow-xl" onClick={(e) => e.stopPropagation()}>
            <div className="flex items-center gap-3 mb-4">
              <div className="p-2 rounded-full bg-destructive/10">
                <AlertTriangle size={20} className="text-destructive" />
              </div>
              <h3 className="font-semibold">{t('sidebar.deleteSession')}</h3>
            </div>
            <p className="text-sm text-muted-foreground mb-1">{t('sidebar.deleteSessionConfirm')}</p>
            <p className="text-sm font-medium mb-6 truncate">&ldquo;{deleteConfirm.name}&rdquo;</p>
            <div className="flex justify-end gap-2">
              <button
                onClick={() => setDeleteConfirm(null)}
                className="px-4 py-1.5 text-sm rounded-lg border border-border hover:bg-accent"
              >
                {t('common.cancel')}
              </button>
              <button
                onClick={confirmDeleteSession}
                className="px-4 py-1.5 text-sm rounded-lg bg-destructive text-destructive-foreground hover:bg-destructive/90"
              >
                {t('common.delete')}
              </button>
            </div>
          </div>
        </div>
      )}
    </aside>
  );
}
