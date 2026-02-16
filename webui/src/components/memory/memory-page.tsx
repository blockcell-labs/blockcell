import { useEffect, useState } from 'react';
import { Search, Plus, Trash2, RefreshCw, Brain, Loader2, AlertTriangle } from 'lucide-react';
import { cn } from '@/lib/utils';
import { getMemories, createMemory, deleteMemory, getMemoryStats } from '@/lib/api';
import { useT } from '@/lib/i18n';

export function MemoryPage() {
  const t = useT();
  const [memories, setMemories] = useState<any[]>([]);
  const [stats, setStats] = useState<any>(null);
  const [query, setQuery] = useState('');
  const [scope, setScope] = useState('');
  const [loading, setLoading] = useState(true);
  const [showCreate, setShowCreate] = useState(false);
  const [newMemory, setNewMemory] = useState({ title: '', content: '', scope: 'long_term', type: 'note', tags: '' });
  const [deleteConfirm, setDeleteConfirm] = useState<{ id: string; title: string } | null>(null);

  useEffect(() => {
    fetchMemories();
    fetchStats();
  }, []);

  async function fetchMemories() {
    setLoading(true);
    try {
      const data = await getMemories({ q: query || undefined, scope: scope || undefined, limit: 50 });
      const raw = Array.isArray(data) ? data : data.results || data.items || [];
      // API returns [{ item: {...}, score }] — unwrap .item if present
      setMemories(raw.map((entry: any) => entry.item ? { ...entry.item, _score: entry.score } : entry));
    } catch {
      setMemories([]);
    } finally {
      setLoading(false);
    }
  }

  async function fetchStats() {
    try {
      const data = await getMemoryStats();
      setStats(data);
    } catch {
      // ignore
    }
  }

  async function handleCreate() {
    try {
      await createMemory({
        title: newMemory.title,
        content: newMemory.content,
        scope: newMemory.scope,
        type: newMemory.type,
        tags: newMemory.tags,
      });
      setShowCreate(false);
      setNewMemory({ title: '', content: '', scope: 'long_term', type: 'note', tags: '' });
      fetchMemories();
      fetchStats();
    } catch {
      // ignore
    }
  }

  function requestDelete(id: string, title: string) {
    setDeleteConfirm({ id, title });
  }

  async function confirmDelete() {
    if (!deleteConfirm) return;
    try {
      await deleteMemory(deleteConfirm.id);
      setMemories((prev) => prev.filter((m) => m.id !== deleteConfirm.id));
      fetchStats();
    } catch {
      // ignore
    } finally {
      setDeleteConfirm(null);
    }
  }

  function handleSearch(e: React.FormEvent) {
    e.preventDefault();
    fetchMemories();
  }

  return (
    <div className="flex flex-col h-full">
      <div className="border-b border-border px-6 py-4 flex items-center justify-between">
        <div>
          <h1 className="text-lg font-semibold">{t('memory.title')}</h1>
          {stats && (
            <p className="text-sm text-muted-foreground">
              {stats.total_active} active · {stats.long_term} long-term · {stats.short_term} short-term
            </p>
          )}
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={() => setShowCreate(!showCreate)}
            className="flex items-center gap-1.5 px-3 py-1.5 text-sm rounded-lg bg-primary text-primary-foreground hover:bg-primary/90"
          >
            <Plus size={14} /> {t('memory.addMemory')}
          </button>
          <button
            onClick={() => { fetchMemories(); fetchStats(); }}
            className="p-2 rounded-lg hover:bg-accent text-muted-foreground"
          >
            <RefreshCw size={16} className={loading ? 'animate-spin' : ''} />
          </button>
        </div>
      </div>

      {/* Create form */}
      {showCreate && (
        <div className="border-b border-border p-4 bg-card/50 space-y-3">
          <div className="grid grid-cols-2 gap-3">
            <input
              value={newMemory.title}
              onChange={(e) => setNewMemory({ ...newMemory, title: e.target.value })}
              placeholder="Title"
              className="px-3 py-1.5 text-sm bg-background border border-border rounded-lg outline-none focus:ring-1 focus:ring-ring"
            />
            <input
              value={newMemory.tags}
              onChange={(e) => setNewMemory({ ...newMemory, tags: e.target.value })}
              placeholder="Tags (comma-separated)"
              className="px-3 py-1.5 text-sm bg-background border border-border rounded-lg outline-none focus:ring-1 focus:ring-ring"
            />
          </div>
          <textarea
            value={newMemory.content}
            onChange={(e) => setNewMemory({ ...newMemory, content: e.target.value })}
            placeholder="Content"
            rows={3}
            className="w-full px-3 py-1.5 text-sm bg-background border border-border rounded-lg outline-none focus:ring-1 focus:ring-ring resize-none"
          />
          <div className="flex items-center gap-3">
            <select
              value={newMemory.scope}
              onChange={(e) => setNewMemory({ ...newMemory, scope: e.target.value })}
              className="px-3 py-1.5 text-sm bg-background border border-border rounded-lg outline-none"
            >
              <option value="long_term">Long-term</option>
              <option value="short_term">Short-term</option>
            </select>
            <select
              value={newMemory.type}
              onChange={(e) => setNewMemory({ ...newMemory, type: e.target.value })}
              className="px-3 py-1.5 text-sm bg-background border border-border rounded-lg outline-none"
            >
              {['note', 'fact', 'preference', 'project', 'task', 'glossary', 'contact', 'snippet', 'policy', 'summary'].map((tp) => (
                <option key={tp} value={tp}>{tp}</option>
              ))}
            </select>
            <button
              onClick={handleCreate}
              disabled={!newMemory.content}
              className="px-4 py-1.5 text-sm rounded-lg bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
            >
              {t('common.save')}
            </button>
          </div>
        </div>
      )}

      {/* Search bar */}
      <form onSubmit={handleSearch} className="px-6 py-3 flex items-center gap-2">
        <div className="flex-1 flex items-center gap-2 bg-card border border-border rounded-lg px-3 py-1.5">
          <Search size={14} className="text-muted-foreground" />
          <input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder={t('memory.searchPlaceholder')}
            className="flex-1 bg-transparent text-sm outline-none"
          />
        </div>
        <select
          value={scope}
          onChange={(e) => { setScope(e.target.value); }}
          className="px-3 py-1.5 text-sm bg-card border border-border rounded-lg outline-none"
        >
          <option value="">All scopes</option>
          <option value="long_term">Long-term</option>
          <option value="short_term">Short-term</option>
        </select>
        <button type="submit" className="px-3 py-1.5 text-sm rounded-lg bg-accent hover:bg-accent/80">
          {t('common.search')}
        </button>
      </form>

      {/* Results */}
      <div className="flex-1 overflow-y-auto px-6 pb-6">
        {loading ? (
          <div className="flex items-center justify-center h-32">
            <Loader2 size={24} className="animate-spin text-muted-foreground" />
          </div>
        ) : memories.length === 0 ? (
          <div className="flex flex-col items-center justify-center h-32 text-muted-foreground">
            <Brain size={32} className="mb-2 opacity-30" />
            <p className="text-sm">{t('memory.empty')}</p>
          </div>
        ) : (
          <div className="space-y-2">
            {memories.map((mem: any, idx: number) => (
              <div key={mem.id || `mem_${idx}`} className="group border border-border rounded-lg p-3 bg-card">
                <div className="flex items-start justify-between gap-2">
                  <div className="flex-1 min-w-0">
                    <div className="flex items-center gap-2 flex-wrap">
                      {mem.title && <span className="font-medium text-sm">{mem.title}</span>}
                      <span className="text-[10px] px-1.5 py-0.5 rounded bg-muted text-muted-foreground">{mem.scope}</span>
                      <span className="text-[10px] px-1.5 py-0.5 rounded bg-muted text-muted-foreground">{mem.type || mem.item_type}</span>
                    </div>
                    <p className="text-sm text-muted-foreground mt-1 line-clamp-3">{mem.content}</p>
                    {mem.tags && mem.tags.length > 0 && (
                      <div className="flex gap-1 mt-1.5 flex-wrap">
                        {(Array.isArray(mem.tags) ? mem.tags : String(mem.tags).split(',')).filter(Boolean).map((tag: string, i: number) => (
                          <span key={i} className="text-[10px] px-1.5 py-0.5 rounded-full bg-rust/10 text-rust">
                            {String(tag).trim()}
                          </span>
                        ))}
                      </div>
                    )}
                  </div>
                  <button
                    onClick={() => requestDelete(mem.id, mem.title || mem.content?.slice(0, 30) || mem.id)}
                    className="opacity-0 group-hover:opacity-100 p-1 rounded hover:bg-destructive/20 text-destructive transition-opacity"
                  >
                    <Trash2 size={14} />
                  </button>
                </div>
              </div>
            ))}
          </div>
        )}
      </div>
      {/* Delete confirmation dialog */}
      {deleteConfirm && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50" onClick={() => setDeleteConfirm(null)}>
          <div className="bg-card border border-border rounded-xl p-6 max-w-sm w-full mx-4 shadow-xl" onClick={(e) => e.stopPropagation()}>
            <div className="flex items-center gap-3 mb-4">
              <div className="p-2 rounded-full bg-destructive/10">
                <AlertTriangle size={20} className="text-destructive" />
              </div>
              <h3 className="font-semibold">{t('memory.deleteTitle')}</h3>
            </div>
            <p className="text-sm text-muted-foreground mb-1">{t('memory.deleteConfirm')}</p>
            <p className="text-sm font-medium mb-6 truncate">&ldquo;{deleteConfirm.title}&rdquo;</p>
            <div className="flex justify-end gap-2">
              <button
                onClick={() => setDeleteConfirm(null)}
                className="px-4 py-1.5 text-sm rounded-lg border border-border hover:bg-accent"
              >
                {t('common.cancel')}
              </button>
              <button
                onClick={confirmDelete}
                className="px-4 py-1.5 text-sm rounded-lg bg-destructive text-destructive-foreground hover:bg-destructive/90"
              >
                {t('common.delete')}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
