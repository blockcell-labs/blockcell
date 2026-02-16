import { useEffect, useState, useCallback, useRef } from 'react';
import { Activity, Cpu, Brain, Zap, RefreshCw, Shield, GitBranch } from 'lucide-react';
import { getHealth, getTools, getSkills, getEvolution, getStats, getToggles, updateToggle } from '@/lib/api';
import { useT } from '@/lib/i18n';
import { wsManager } from '@/lib/ws';

export function DashboardPage() {
  const t = useT();
  const [health, setHealth] = useState<any>(null);
  const [tools, setTools] = useState<any[]>([]);
  const [skills, setSkills] = useState<any[]>([]);
  const [evolution, setEvolution] = useState<any[]>([]);
  const [stats, setStats] = useState<any>(null);
  const [loading, setLoading] = useState(true);
  const [toggles, setToggles] = useState<{ skills: Record<string, boolean>; tools: Record<string, boolean> }>({ skills: {}, tools: {} });
  const fetchAllRef = useRef(fetchAll);
  fetchAllRef.current = fetchAll;

  useEffect(() => {
    fetchAll();
    const interval = setInterval(fetchAll, 15000);
    // Auto-refresh when skills are created/updated via chat
    const offSkills = wsManager.on('skills_updated', () => {
      fetchAllRef.current();
    });
    return () => { clearInterval(interval); offSkills(); };
  }, []);

  async function fetchAll() {
    try {
      const [h, c, s, e, st, tg] = await Promise.allSettled([
        getHealth(),
        getTools(),
        getSkills(),
        getEvolution(),
        getStats(),
        getToggles(),
      ]);
      if (h.status === 'fulfilled') setHealth(h.value);
      if (c.status === 'fulfilled') setTools(c.value.tools || []);
      if (s.status === 'fulfilled') setSkills(s.value.skills || []);
      if (e.status === 'fulfilled') setEvolution(e.value.records || []);
      if (st.status === 'fulfilled') setStats(st.value);
      if (tg.status === 'fulfilled') setToggles(tg.value);
    } finally {
      setLoading(false);
    }
  }

  const isEnabled = useCallback((category: 'skills' | 'tools', name: string) => {
    const map = toggles[category] || {};
    return map[name] !== false; // default is enabled (missing = true)
  }, [toggles]);

  const handleToggle = useCallback(async (category: 'skills' | 'tools', name: string) => {
    const current = isEnabled(category, name);
    const newVal = !current;
    // Optimistic update
    setToggles(prev => ({
      ...prev,
      [category]: { ...prev[category], [name]: newVal },
    }));
    try {
      await updateToggle(category, name, newVal);
    } catch {
      // Revert on error
      setToggles(prev => ({
        ...prev,
        [category]: { ...prev[category], [name]: current },
      }));
    }
  }, [isEnabled]);

  function formatUptime(secs: number): string {
    const h = Math.floor(secs / 3600);
    const m = Math.floor((secs % 3600) / 60);
    return h > 0 ? `${h}h ${m}m` : `${m}m`;
  }

  return (
    <div className="flex flex-col h-full overflow-y-auto">
      <div className="border-b border-border px-6 py-4 flex items-center justify-between">
        <h1 className="text-lg font-semibold">{t('dashboard.title')}</h1>
        <button
          onClick={() => { setLoading(true); fetchAll(); }}
          className="p-2 rounded-lg hover:bg-accent text-muted-foreground"
        >
          <RefreshCw size={16} className={loading ? 'animate-spin' : ''} />
        </button>
      </div>

      <div className="p-6 space-y-6 w-full">
        {/* Health cards */}
        <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-4">
          <StatCard
            icon={<Activity size={20} />}
            label={t('dashboard.status')}
            value={health?.status || '—'}
            color={health?.status === 'ok' ? 'text-cyber' : 'text-red-500'}
          />
          <StatCard
            icon={<Cpu size={20} />}
            label={t('dashboard.model')}
            value={health?.model || '—'}
          />
          <StatCard
            icon={<Zap size={20} />}
            label={t('dashboard.uptime')}
            value={health ? formatUptime(health.uptime_secs) : '—'}
          />
          <StatCard
            icon={<Shield size={20} />}
            label={t('dashboard.version')}
            value={health?.version || '—'}
          />
        </div>

        {/* Stats */}
        {stats && (
          <div className="grid grid-cols-1 sm:grid-cols-3 gap-4">
            <StatCard icon={<Brain size={20} />} label={t('dashboard.memoryItems')} value={stats.memory_items != null ? String(stats.memory_items) : '—'} />
            <StatCard icon={<GitBranch size={20} />} label={t('dashboard.evolutionRecords')} value={String(evolution.length)} />
            <StatCard icon={<Zap size={20} />} label={t('dashboard.activeTasks')} value={stats.active_tasks != null ? String(stats.active_tasks) : '—'} />
          </div>
        )}

        {/* Skills */}
        <section>
          <h2 className="text-sm font-semibold mb-3 text-muted-foreground uppercase tracking-wider font-mono">
            <span className="text-rust">▸</span> {t('dashboard.skills')} ({skills.length})
          </h2>
          <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-2">
            {skills.map((skill: any, i: number) => {
              const enabled = isEnabled('skills', skill.name);
              return (
                <div key={i} className={`border border-border rounded-lg p-3 bg-card text-sm transition-opacity ${enabled ? '' : 'opacity-50'}`}>
                  <div className="flex items-center justify-between gap-2">
                    <div className="flex items-center gap-2 min-w-0">
                      <span className="font-medium">{skill.name}</span>
                      <span className="text-[10px] px-1.5 py-0.5 rounded bg-muted text-muted-foreground">
                        {skill.source}
                      </span>
                    </div>
                    <ToggleSwitch enabled={enabled} onChange={() => handleToggle('skills', skill.name)} />
                  </div>
                </div>
              );
            })}
            {skills.length === 0 && (
              <p className="text-sm text-muted-foreground col-span-full">{t('dashboard.noSkills')}</p>
            )}
          </div>
        </section>

        {/* Recent evolution */}
        {evolution.length > 0 && (
          <section>
            <h2 className="text-sm font-semibold mb-3 text-muted-foreground uppercase tracking-wider font-mono">
              <span className="text-cyber">▸</span> {t('dashboard.recentEvolution')} ({evolution.length})
            </h2>
            <div className="space-y-2">
              {evolution.slice(0, 10).map((rec: any, i: number) => (
                <div key={i} className="border border-border rounded-lg p-3 bg-card text-sm flex items-center gap-3">
                  <GitBranch size={14} className="text-muted-foreground shrink-0" />
                  <div className="flex-1 min-w-0">
                    <span className="font-medium">{rec.skill_name || rec.id}</span>
                    {rec.status && (
                      <span className="ml-2 text-xs text-muted-foreground">{rec.status}</span>
                    )}
                  </div>
                </div>
              ))}
            </div>
          </section>
        )}

        {/* Tools */}
        <section>
          <h2 className="text-sm font-semibold mb-3 text-muted-foreground uppercase tracking-wider font-mono">
            <span className="text-rust">▸</span> {t('dashboard.tools')} ({tools.length})
          </h2>
          <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-2">
            {tools.map((tool: any, i: number) => {
              const name = tool.name || tool;
              const enabled = isEnabled('tools', name);
              return (
                <div key={i} className={`border border-border rounded-lg p-3 bg-card text-sm relative transition-opacity ${enabled ? '' : 'opacity-50'}`}>
                  <div className="flex items-start justify-between gap-2">
                    <div className="flex-1 min-w-0">
                      <div className="font-medium">{name}</div>
                      {tool.description && (
                        <p className="text-xs text-muted-foreground mt-1 line-clamp-2">{tool.description}</p>
                      )}
                    </div>
                    <ToggleSwitch enabled={enabled} onChange={() => handleToggle('tools', name)} />
                  </div>
                </div>
              );
            })}
            {tools.length === 0 && (
              <p className="text-sm text-muted-foreground col-span-full">{t('dashboard.noTools')}</p>
            )}
          </div>
        </section>
      </div>
    </div>
  );
}

function ToggleSwitch({ enabled, onChange }: { enabled: boolean; onChange: () => void }) {
  return (
    <button
      onClick={(e) => { e.stopPropagation(); onChange(); }}
      className={`shrink-0 px-2 py-0.5 rounded text-[10px] font-bold uppercase tracking-wider border transition-all duration-150 cursor-pointer select-none ${
        enabled
          ? 'bg-cyber/15 text-cyber border-cyber/40 hover:bg-cyber/25'
          : 'bg-red-500/15 text-red-400 border-red-500/40 hover:bg-red-500/25'
      }`}
      role="switch"
      aria-checked={enabled}
    >
      {enabled ? 'ON' : 'OFF'}
    </button>
  );
}

function StatCard({
  icon,
  label,
  value,
  color,
}: {
  icon: React.ReactNode;
  label: string;
  value: string;
  color?: string;
}) {
  return (
    <div className="border border-border rounded-lg p-4 bg-card">
      <div className="flex items-center gap-3">
        <div className="text-muted-foreground">{icon}</div>
        <div>
          <p className="text-xs text-muted-foreground">{label}</p>
          <p className={`text-sm font-semibold ${color || ''}`}>{value}</p>
        </div>
      </div>
    </div>
  );
}
