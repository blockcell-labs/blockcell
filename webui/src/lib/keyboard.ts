// Global keyboard shortcuts for the WebUI
import { useSidebarStore } from './store';

type ShortcutHandler = () => void;

interface Shortcut {
  key: string;
  ctrl?: boolean;
  meta?: boolean;
  shift?: boolean;
  alt?: boolean;
  description: string;
  handler: ShortcutHandler;
}

const shortcuts: Shortcut[] = [];

export function registerShortcuts() {
  const { setActivePage, toggle } = useSidebarStore.getState();

  shortcuts.length = 0;
  shortcuts.push(
    { key: '1', alt: true, description: 'Go to Chat', handler: () => setActivePage('chat') },
    { key: '2', alt: true, description: 'Go to Tasks', handler: () => setActivePage('tasks') },
    { key: '3', alt: true, description: 'Go to Dashboard', handler: () => setActivePage('dashboard') },
    { key: '4', alt: true, description: 'Go to Memory', handler: () => setActivePage('memory') },
    { key: '5', alt: true, description: 'Go to Cron Jobs', handler: () => setActivePage('cron') },
    { key: '6', alt: true, description: 'Go to Alerts', handler: () => setActivePage('alerts') },
    { key: '7', alt: true, description: 'Go to Streams', handler: () => setActivePage('streams') },
    { key: '8', alt: true, description: 'Go to Files', handler: () => setActivePage('files') },
    { key: '9', alt: true, description: 'Go to Settings', handler: () => setActivePage('config') },
    { key: 'b', ctrl: true, description: 'Toggle sidebar', handler: () => toggle() },
    { key: 'b', meta: true, description: 'Toggle sidebar', handler: () => toggle() },
  );
}

export function handleGlobalKeyDown(e: KeyboardEvent) {
  // Don't intercept when typing in inputs
  const tag = (e.target as HTMLElement)?.tagName;
  if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') {
    // Allow Alt shortcuts even in inputs
    if (!e.altKey) return;
  }

  for (const s of shortcuts) {
    const ctrlMatch = s.ctrl ? (e.ctrlKey || e.metaKey) : true;
    const metaMatch = s.meta ? e.metaKey : true;
    const shiftMatch = s.shift ? e.shiftKey : true;
    const altMatch = s.alt ? e.altKey : true;

    // Ensure we don't match when modifier isn't pressed
    if (s.alt && !e.altKey) continue;
    if (s.ctrl && !e.ctrlKey && !e.metaKey) continue;
    if (s.meta && !e.metaKey) continue;

    if (e.key === s.key && ctrlMatch && metaMatch && shiftMatch && altMatch) {
      e.preventDefault();
      s.handler();
      return;
    }
  }
}

export function getShortcutsList(): { key: string; description: string }[] {
  return shortcuts.map((s) => {
    const parts: string[] = [];
    if (s.ctrl) parts.push('Ctrl');
    if (s.meta) parts.push('Cmd');
    if (s.alt) parts.push('Alt');
    if (s.shift) parts.push('Shift');
    parts.push(s.key.toUpperCase());
    return { key: parts.join('+'), description: s.description };
  });
}
