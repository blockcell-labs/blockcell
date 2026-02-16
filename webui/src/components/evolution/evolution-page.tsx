import { useEffect, useState, useCallback, useRef } from 'react';
import {
  GitBranch, RefreshCw, Dna, Zap, CheckCircle, XCircle, Clock,
  Plus, Trash2, ChevronDown, ChevronRight, AlertTriangle, FlaskConical,
  ArrowUpCircle, RotateCcw, Send, Loader2, Eye, EyeOff, Cpu, Code2,
  History, FileCode, Tag, Search, ThumbsUp, ThumbsDown, Sparkles, Wrench,
} from 'lucide-react';
import {
  getEvolution, getEvolutionToolEvolutions, getSkills, getTools,
  triggerEvolution, deleteEvolution, testSkill, getTestSuggestion, getEvolutionSummary,
  getToolEvolutionVersions, searchSkills,
  type EvolutionRecord, type CoreEvolutionRecord, type EvolutionSummary,
} from '@/lib/api';
import { useT } from '@/lib/i18n';
import { wsManager } from '@/lib/ws';

type Tab = 'overview' | 'skills' | 'tool-evolutions' | 'test';

// ── Status helpers ──

function statusColor(status: string): string {
  switch (status) {
    case 'Completed': case 'Active': case 'TestPassed': return 'text-cyber';
    case 'Triggered': case 'Requested': return 'text-blue-400';
    case 'Generating': case 'Compiling': case 'Auditing': case 'Validating':
    case 'RollingOut': return 'text-yellow-400';
    case 'Failed': case 'AuditFailed': case 'DryRunFailed':
    case 'TestFailed': case 'RolledBack': case 'Blocked': return 'text-red-400';
    default: return 'text-muted-foreground';
  }
}

function statusIcon(status: string) {
  switch (status) {
    case 'Completed': case 'Active': case 'TestPassed':
      return <CheckCircle size={14} className="text-cyber" />;
    case 'Triggered': case 'Requested':
      return <Zap size={14} className="text-blue-400" />;
    case 'Generating': case 'Compiling': case 'Auditing': case 'Validating':
    case 'RollingOut':
      return <Loader2 size={14} className="text-yellow-400 animate-spin" />;
    case 'Failed': case 'AuditFailed': case 'DryRunFailed':
    case 'TestFailed': case 'RolledBack': case 'Blocked':
      return <XCircle size={14} className="text-red-400" />;
    default:
      return <Clock size={14} className="text-muted-foreground" />;
  }
}

function formatTime(ts: number): string {
  if (!ts) return '—';
  const d = new Date(ts * 1000);
  const now = new Date();
  const diffMs = now.getTime() - d.getTime();
  const diffMin = Math.floor(diffMs / 60000);
  if (diffMin < 1) return 'just now';
  if (diffMin < 60) return `${diffMin}m ago`;
  const diffH = Math.floor(diffMin / 60);
  if (diffH < 24) return `${diffH}h ago`;
  const diffD = Math.floor(diffH / 24);
  return `${diffD}d ago`;
}

function triggerLabel(trigger: any): string {
  if (!trigger) return 'unknown';
  if (trigger.ManualRequest) return 'Manual';
  if (trigger.ExecutionError) return 'Error';
  if (trigger.PerformanceDegradation) return 'Performance';
  if (trigger.UserFeedback) return 'Feedback';
  if (typeof trigger === 'string') return trigger;
  return Object.keys(trigger)[0] || 'unknown';
}

// ── Pipeline stage visualization ──

const SKILL_STAGES = [
  'Triggered', 'Generating', 'Auditing', 'TestPassed', 'RollingOut', 'Completed',
];

const CAP_STAGES = [
  'Requested', 'Generating', 'Compiling', 'Validating', 'Active',
];

function PipelineStages({ status, stages }: { status: string; stages: string[] }) {
  const currentIdx = stages.indexOf(status);
  const isFailed = ['Failed', 'AuditFailed', 'DryRunFailed', 'TestFailed', 'RolledBack', 'Blocked'].includes(status);

  return (
    <div className="flex items-center gap-1">
      {stages.map((stage, i) => {
        const isActive = stage === status;
        const isPast = currentIdx >= 0 && i < currentIdx;
        const isCompleted = status === 'Completed' || status === 'Active';

        let dotClass = 'w-2 h-2 rounded-full transition-all ';
        if (isCompleted && i <= stages.length - 1) {
          dotClass += 'bg-cyber';
        } else if (isActive && isFailed) {
          dotClass += 'bg-red-400';
        } else if (isActive) {
          dotClass += 'bg-yellow-400 animate-pulse';
        } else if (isPast) {
          dotClass += 'bg-cyber';
        } else {
          dotClass += 'bg-muted';
        }

        return (
          <div key={stage} className="flex items-center gap-1">
            <div className={dotClass} title={stage} />
            {i < stages.length - 1 && (
              <div className={`w-3 h-px ${isPast || (isCompleted) ? 'bg-cyber' : 'bg-muted'}`} />
            )}
          </div>
        );
      })}
    </div>
  );
}

// ── Main component ──

export function EvolutionPage() {
  const t = useT();
  const [tab, setTab] = useState<Tab>('overview');
  const [summary, setSummary] = useState<EvolutionSummary | null>(null);
  const [skillRecords, setSkillRecords] = useState<EvolutionRecord[]>([]);
  const [capRecords, setCapRecords] = useState<CoreEvolutionRecord[]>([]);
  const [skills, setSkills] = useState<any[]>([]);
  const [toolCount, setToolCount] = useState(0);
  const [loading, setLoading] = useState(true);
  const [expandedId, setExpandedId] = useState<string | null>(null);

  // New skill creation flow
  const [showNewSkill, setShowNewSkill] = useState(false);
  const [newSkillDesc, setNewSkillDesc] = useState('');
  const [newSkillName, setNewSkillName] = useState('');
  const [searchingSkills, setSearchingSkills] = useState(false);
  const [searchResults, setSearchResults] = useState<any[] | null>(null);
  const [creatingSkill, setCreatingSkill] = useState(false);

  // Test & Evolve form (merged)
  const [testSkillName, setTestSkillName] = useState('');
  const [testInput, setTestInput] = useState('');
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<any>(null);
  const [loadingSuggestion, setLoadingSuggestion] = useState(false);
  const [showEvolveForm, setShowEvolveForm] = useState(false);
  const [evolveDesc, setEvolveDesc] = useState('');
  const [triggeringEvolve, setTriggeringEvolve] = useState(false);

  // Filter
  const [statusFilter, setStatusFilter] = useState<string>('all');

  const fetchAllRef = useRef(fetchAll);
  fetchAllRef.current = fetchAll;

  useEffect(() => {
    fetchAll();
    const interval = setInterval(fetchAll, 10000);
    const offSkills = wsManager.on('skills_updated', () => fetchAllRef.current());
    return () => { clearInterval(interval); offSkills(); };
  }, []);

  async function fetchAll() {
    try {
      const [evo, cap, sk, sum, tls] = await Promise.allSettled([
        getEvolution(),
        getEvolutionToolEvolutions(),
        getSkills(),
        getEvolutionSummary(),
        getTools(),
      ]);
      if (evo.status === 'fulfilled') {
        const recs = (evo.value.records || []) as EvolutionRecord[];
        recs.sort((a, b) => (b.updated_at || 0) - (a.updated_at || 0));
        setSkillRecords(recs);
      }
      if (cap.status === 'fulfilled') {
        setCapRecords(cap.value.records || []);
      }
      if (sk.status === 'fulfilled') {
        setSkills(sk.value.skills || []);
      }
      if (sum.status === 'fulfilled') {
        setSummary(sum.value);
      }
      if (tls.status === 'fulfilled') {
        setToolCount(tls.value.count || 0);
      }
    } finally {
      setLoading(false);
    }
  }

  // New skill: search for similar skills first
  const handleNewSkillSearch = useCallback(async () => {
    if (!newSkillDesc.trim()) return;
    setSearchingSkills(true);
    setSearchResults(null);
    try {
      const res = await searchSkills(newSkillDesc.trim());
      setSearchResults(res.results || []);
    } catch {
      setSearchResults([]);
    } finally {
      setSearchingSkills(false);
    }
  }, [newSkillDesc]);

  // New skill: create via evolution trigger
  const handleCreateNewSkill = useCallback(async () => {
    if (!newSkillName.trim() || !newSkillDesc.trim()) return;
    setCreatingSkill(true);
    try {
      await triggerEvolution(newSkillName.trim(), newSkillDesc.trim());
      setNewSkillDesc('');
      setNewSkillName('');
      setSearchResults(null);
      setShowNewSkill(false);
      fetchAll();
    } finally {
      setCreatingSkill(false);
    }
  }, [newSkillName, newSkillDesc]);

  // Use existing skill → jump to test tab
  const handleUseExisting = useCallback((skillName: string) => {
    setShowNewSkill(false);
    setSearchResults(null);
    setTestSkillName(skillName);
    setTestResult(null);
    setShowEvolveForm(false);
    setTab('test');
    // Auto-load suggestion
    setLoadingSuggestion(true);
    getTestSuggestion(skillName).then(res => {
      if (res.suggestion) setTestInput(res.suggestion);
    }).catch(() => {}).finally(() => setLoadingSuggestion(false));
  }, []);

  const handleDelete = useCallback(async (id: string) => {
    await deleteEvolution(id);
    fetchAll();
  }, []);

  const handleSkillSelect = useCallback(async (name: string) => {
    setTestSkillName(name);
    setTestInput('');
    setTestResult(null);
    setShowEvolveForm(false);
    setEvolveDesc('');
    if (!name) return;
    setLoadingSuggestion(true);
    try {
      const res = await getTestSuggestion(name);
      if (res.suggestion) {
        setTestInput(res.suggestion);
      }
    } catch {
      // Silently ignore — user can still type manually
    } finally {
      setLoadingSuggestion(false);
    }
  }, []);

  const handleTest = useCallback(async () => {
    if (!testSkillName.trim() || !testInput.trim()) return;
    setTesting(true);
    setTestResult(null);
    setShowEvolveForm(false);
    try {
      const result = await testSkill(testSkillName.trim(), testInput.trim());
      setTestResult(result);
    } catch (e: any) {
      setTestResult({ error: e.message || 'Unknown error' });
    } finally {
      setTesting(false);
    }
  }, [testSkillName, testInput]);

  // Trigger evolution from test result
  const handleEvolveFromTest = useCallback(async () => {
    if (!testSkillName.trim() || !evolveDesc.trim()) return;
    setTriggeringEvolve(true);
    try {
      const fullDesc = `[Test feedback] Input: ${testInput}\nResult: ${testResult?.result || testResult?.error || 'N/A'}\n\nImprovement needed: ${evolveDesc}`;
      await triggerEvolution(testSkillName.trim(), fullDesc);
      setShowEvolveForm(false);
      setEvolveDesc('');
      fetchAll();
    } finally {
      setTriggeringEvolve(false);
    }
  }, [testSkillName, testInput, testResult, evolveDesc]);

  // Filtered records
  const filteredSkillRecords = statusFilter === 'all'
    ? skillRecords
    : statusFilter === 'active'
      ? skillRecords.filter(r => !['Completed', 'Failed', 'RolledBack', 'AuditFailed', 'DryRunFailed', 'TestFailed'].includes(r.status))
      : statusFilter === 'completed'
        ? skillRecords.filter(r => r.status === 'Completed')
        : skillRecords.filter(r => ['Failed', 'RolledBack', 'AuditFailed', 'DryRunFailed', 'TestFailed'].includes(r.status));

  const filteredCapRecords = statusFilter === 'all'
    ? capRecords
    : statusFilter === 'active'
      ? capRecords.filter(r => !['Active', 'Failed', 'Blocked'].includes(r.status))
      : statusFilter === 'completed'
        ? capRecords.filter(r => r.status === 'Active')
        : capRecords.filter(r => ['Failed', 'Blocked'].includes(r.status));

  // Stats
  const activeSkills = skillRecords.filter(r => !['Completed', 'Failed', 'RolledBack', 'AuditFailed', 'DryRunFailed', 'TestFailed'].includes(r.status)).length;
  const completedSkills = skillRecords.filter(r => r.status === 'Completed').length;
  const failedSkills = skillRecords.filter(r => ['Failed', 'RolledBack', 'AuditFailed', 'DryRunFailed', 'TestFailed'].includes(r.status)).length;
  const activeCaps = capRecords.filter(r => !['Active', 'Failed', 'Blocked'].includes(r.status)).length;
  const completedCaps = capRecords.filter(r => r.status === 'Active').length;

  // All skills for test dropdown
  const allSkillNames = [
    ...new Set(skillRecords.filter(r => r.status === 'Completed').map(r => r.skill_name)),
    ...skills.map(s => s.name),
  ].filter((v, i, a) => a.indexOf(v) === i);

  return (
    <div className="flex flex-col h-full overflow-y-auto">
      {/* Header */}
      <div className="border-b border-border px-6 py-4 flex items-center justify-between">
        <div className="flex items-center gap-3">
          <Dna size={20} className="text-rust" />
          <h1 className="text-lg font-semibold">{t('evolution.title')}</h1>
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={() => { setShowNewSkill(!showNewSkill); setSearchResults(null); setNewSkillDesc(''); setNewSkillName(''); }}
            className="flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded-lg bg-cyber/10 text-cyber border border-cyber/30 hover:bg-cyber/20 transition-colors"
          >
            <Plus size={12} />
            {t('evolution.triggerNew')}
          </button>
          <button
            onClick={() => { setLoading(true); fetchAll(); }}
            className="p-2 rounded-lg hover:bg-accent text-muted-foreground"
          >
            <RefreshCw size={16} className={loading ? 'animate-spin' : ''} />
          </button>
        </div>
      </div>

      <div className="p-6 space-y-6 w-full">
        {/* Stats cards */}
        <div className="grid grid-cols-2 sm:grid-cols-3 lg:grid-cols-6 gap-3">
          <MiniStat label={t('evolution.totalRecords')} value={String(skillRecords.length + capRecords.length)} icon={<GitBranch size={16} />} />
          <MiniStat label={t('evolution.evolving')} value={String(activeSkills + activeCaps)} icon={<Loader2 size={16} className="animate-spin" />} color="text-yellow-400" />
          <MiniStat label={t('evolution.completed')} value={String(completedSkills + completedCaps)} icon={<CheckCircle size={16} />} color="text-cyber" />
          <MiniStat label={t('evolution.failed')} value={String(failedSkills)} icon={<XCircle size={16} />} color="text-red-400" />
          <MiniStat label={t('evolution.skills')} value={String(skills.length)} icon={<Code2 size={16} />} />
          <MiniStat label={t('evolution.tools')} value={String(toolCount)} icon={<Wrench size={16} />} color="text-purple-400" />
        </div>

        {/* New Skill creation form */}
        {showNewSkill && (
          <div className="border border-cyber/30 rounded-xl p-5 bg-cyber/5 space-y-4">
            <div className="flex items-center justify-between">
              <h3 className="text-sm font-semibold flex items-center gap-2">
                <Sparkles size={14} className="text-cyber" />
                {t('evolution.newSkillTitle')}
              </h3>
              <button onClick={() => setShowNewSkill(false)} className="text-muted-foreground hover:text-foreground p-1">
                <XCircle size={14} />
              </button>
            </div>
            <p className="text-xs text-muted-foreground">{t('evolution.newSkillDesc')}</p>

            {/* Step 1: Describe the skill */}
            <div>
              <label className="text-xs text-muted-foreground mb-1 block">{t('evolution.description')}</label>
              <textarea
                value={newSkillDesc}
                onChange={e => { setNewSkillDesc(e.target.value); setSearchResults(null); }}
                placeholder={t('evolution.newSkillPlaceholder')}
                rows={3}
                className="w-full px-3 py-2 text-sm rounded-lg border border-border bg-background focus:outline-none focus:ring-1 focus:ring-cyber resize-none"
              />
            </div>

            {/* Search button */}
            {!searchResults && (
              <div className="flex justify-end">
                <button
                  onClick={handleNewSkillSearch}
                  disabled={searchingSkills || !newSkillDesc.trim()}
                  className="flex items-center gap-1.5 px-4 py-1.5 text-xs font-medium rounded-lg bg-cyber/15 text-cyber border border-cyber/30 hover:bg-cyber/25 disabled:opacity-50 transition-colors"
                >
                  {searchingSkills ? <Loader2 size={12} className="animate-spin" /> : <Search size={12} />}
                  {searchingSkills ? t('evolution.searchingSkills') : t('common.search')}
                </button>
              </div>
            )}

            {/* Search results */}
            {searchResults !== null && (
              <div className="space-y-3">
                {searchResults.length > 0 ? (
                  <>
                    <div className="flex items-center gap-2 text-xs">
                      <CheckCircle size={12} className="text-yellow-400" />
                      <span className="font-medium text-yellow-400">{t('evolution.similarSkillsFound')}</span>
                      <span className="text-muted-foreground">({searchResults.length})</span>
                    </div>
                    <p className="text-[11px] text-muted-foreground">{t('evolution.similarSkillHint')}</p>
                    <div className="space-y-2 max-h-48 overflow-y-auto">
                      {searchResults.slice(0, 5).map((r, i) => (
                        <div key={i} className="flex items-center gap-3 p-2.5 rounded-lg border border-border bg-card hover:bg-accent/30 transition-colors">
                          <div className="flex-1 min-w-0">
                            <div className="flex items-center gap-2">
                              <span className="font-medium text-sm">{r.name}</span>
                              <span className={`text-[9px] px-1.5 py-0.5 rounded font-medium ${
                                r.source === 'builtin' ? 'bg-blue-400/10 text-blue-400' : 'bg-cyber/10 text-cyber'
                              }`}>{r.source}</span>
                              <span className="text-[9px] text-muted-foreground">
                                {t('evolution.matchScore')}: {r.score}
                              </span>
                            </div>
                            {r.description && (
                              <p className="text-[10px] text-muted-foreground mt-0.5 truncate">{r.description}</p>
                            )}
                            {r.triggers && (
                              <p className="text-[10px] text-muted-foreground/60 mt-0.5 truncate">{r.triggers}</p>
                            )}
                          </div>
                          <button
                            onClick={() => handleUseExisting(r.name)}
                            className="shrink-0 px-2.5 py-1 text-[10px] font-medium rounded-md bg-cyber/15 text-cyber border border-cyber/30 hover:bg-cyber/25 transition-colors"
                          >
                            {t('evolution.useExisting')}
                          </button>
                        </div>
                      ))}
                    </div>
                  </>
                ) : (
                  <div className="flex items-center gap-2 text-xs">
                    <Sparkles size={12} className="text-cyber" />
                    <span className="text-cyber">{t('evolution.noSimilarSkills')}</span>
                  </div>
                )}

                {/* Step 2: Name and create */}
                <div className="border-t border-border/50 pt-3 space-y-3">
                  <div>
                    <label className="text-xs text-muted-foreground mb-1 block">{t('evolution.newSkillName')}</label>
                    <input
                      type="text"
                      value={newSkillName}
                      onChange={e => setNewSkillName(e.target.value)}
                      placeholder={t('evolution.newSkillNamePlaceholder')}
                      className="w-full px-3 py-1.5 text-sm rounded-lg border border-border bg-background focus:outline-none focus:ring-1 focus:ring-cyber font-mono"
                      onKeyDown={e => e.key === 'Enter' && handleCreateNewSkill()}
                    />
                  </div>
                  <div className="flex justify-end gap-2">
                    <button
                      onClick={() => { setSearchResults(null); }}
                      className="px-3 py-1 text-xs rounded-lg border border-border hover:bg-accent"
                    >
                      {t('common.cancel')}
                    </button>
                    <button
                      onClick={handleCreateNewSkill}
                      disabled={creatingSkill || !newSkillName.trim() || !newSkillDesc.trim()}
                      className="flex items-center gap-1.5 px-4 py-1.5 text-xs font-medium rounded-lg bg-cyber text-white hover:bg-cyber/90 disabled:opacity-50 transition-colors"
                    >
                      {creatingSkill ? <Loader2 size={12} className="animate-spin" /> : <Sparkles size={12} />}
                      {searchResults.length > 0 ? t('evolution.proceedCreate') : t('evolution.createSkill')}
                    </button>
                  </div>
                </div>
              </div>
            )}
          </div>
        )}

        {/* Tabs */}
        <div className="flex items-center gap-1 border-b border-border">
          {([
            { id: 'overview' as Tab, label: t('evolution.overview'), count: 0 },
            { id: 'skills' as Tab, label: t('evolution.skillEvolution'), count: skillRecords.length },
            { id: 'tool-evolutions' as Tab, label: t('evolution.capEvolution'), count: capRecords.length },
            { id: 'test' as Tab, label: t('evolution.testAndEvolve'), count: 0 },
          ]).map(item => (
            <button
              key={item.id}
              onClick={() => setTab(item.id)}
              className={`px-4 py-2 text-sm font-medium border-b-2 transition-colors ${
                tab === item.id
                  ? 'border-rust text-rust'
                  : 'border-transparent text-muted-foreground hover:text-foreground'
              }`}
            >
              {item.label}
              {item.count > 0 && (
                <span className="ml-1.5 text-[10px] px-1.5 py-0.5 rounded-full bg-muted">{item.count}</span>
              )}
            </button>
          ))}

          {/* Status filter (for skills/tool-evolutions tabs) */}
          {(tab === 'skills' || tab === 'tool-evolutions') && (
            <div className="ml-auto flex items-center gap-1">
              {['all', 'active', 'completed', 'failed'].map(f => (
                <button
                  key={f}
                  onClick={() => setStatusFilter(f)}
                  className={`px-2 py-1 text-[10px] font-medium rounded uppercase tracking-wider transition-colors ${
                    statusFilter === f
                      ? 'bg-rust/15 text-rust'
                      : 'text-muted-foreground hover:text-foreground'
                  }`}
                >
                  {t(`evolution.filter_${f}`)}
                </button>
              ))}
            </div>
          )}
        </div>

        {/* Tab content */}
        {tab === 'overview' && (
          <OverviewTab
            summary={summary}
            skillRecords={skillRecords}
            capRecords={capRecords}
            t={t}
          />
        )}

        {tab === 'skills' && (
          <div className="space-y-2">
            {filteredSkillRecords.length === 0 ? (
              <EmptyState message={t('evolution.noSkillRecords')} />
            ) : (
              filteredSkillRecords.map(rec => (
                <SkillRecordCard
                  key={rec.id}
                  record={rec}
                  expanded={expandedId === rec.id}
                  onToggle={() => setExpandedId(expandedId === rec.id ? null : rec.id)}
                  onDelete={() => handleDelete(rec.id)}
                  t={t}
                />
              ))
            )}
          </div>
        )}

        {tab === 'tool-evolutions' && (
          <div className="space-y-2">
            {filteredCapRecords.length === 0 ? (
              <EmptyState message={t('evolution.noCapRecords')} />
            ) : (
              filteredCapRecords.map(rec => (
                <CapRecordCard
                  key={rec.id}
                  record={rec}
                  expanded={expandedId === rec.id}
                  onToggle={() => setExpandedId(expandedId === rec.id ? null : rec.id)}
                  onDelete={() => handleDelete(rec.id)}
                  t={t}
                />
              ))
            )}
          </div>
        )}

        {/* Test & Evolve tab — merged test + manual evolution */}
        {tab === 'test' && (
          <div className="space-y-4">
            {/* Test section */}
            <div className="relative border border-border rounded-xl p-5 bg-card space-y-4">
              {/* Loading overlay */}
              {(loadingSuggestion || testing) && (
                <div className="absolute inset-0 z-10 flex flex-col items-center justify-center rounded-xl bg-background/80 backdrop-blur-sm">
                  <Loader2 size={24} className="animate-spin text-cyber mb-3" />
                  <span className="text-xs text-muted-foreground">
                    {loadingSuggestion ? t('evolution.loadingSuggestion') : t('evolution.executingTest')}
                  </span>
                </div>
              )}

              <h3 className="text-sm font-semibold flex items-center gap-2">
                <FlaskConical size={14} className="text-cyber" />
                {t('evolution.testAndEvolveTitle')}
              </h3>
              <p className="text-xs text-muted-foreground">{t('evolution.testAndEvolveDesc')}</p>

              {/* Skill selector + test input */}
              <div className="grid grid-cols-1 gap-3">
                <div>
                  <label className="text-xs text-muted-foreground mb-1 block">{t('evolution.skillName')}</label>
                  <select
                    value={testSkillName}
                    onChange={e => handleSkillSelect(e.target.value)}
                    disabled={loadingSuggestion || testing}
                    className="w-full px-3 py-1.5 text-sm rounded-lg border border-border bg-background focus:outline-none focus:ring-1 focus:ring-cyber disabled:opacity-50"
                  >
                    <option value="">{t('evolution.selectSkill')}</option>
                    {allSkillNames.map(name => (
                      <option key={name} value={name}>{name}</option>
                    ))}
                  </select>
                </div>
                <div>
                  <label className="text-xs text-muted-foreground mb-1 block">
                    {t('evolution.testInput')}
                    {testInput && !loadingSuggestion && (
                      <span className="ml-2 text-[10px] text-cyber/60">{t('evolution.aiSuggested')}</span>
                    )}
                  </label>
                  <textarea
                    value={testInput}
                    onChange={e => setTestInput(e.target.value)}
                    placeholder={t('evolution.testInputPlaceholder')}
                    rows={4}
                    disabled={loadingSuggestion || testing}
                    className="w-full px-3 py-2 text-sm rounded-lg border border-border bg-background focus:outline-none focus:ring-1 focus:ring-cyber resize-none font-mono disabled:opacity-50"
                  />
                </div>
              </div>

              <div className="flex justify-end">
                <button
                  onClick={handleTest}
                  disabled={testing || loadingSuggestion || !testSkillName.trim() || !testInput.trim()}
                  className="flex items-center gap-1.5 px-4 py-1.5 text-xs font-medium rounded-lg bg-cyber/15 text-cyber border border-cyber/30 hover:bg-cyber/25 disabled:opacity-50 transition-colors"
                >
                  {testing ? <Loader2 size={12} className="animate-spin" /> : <Send size={12} />}
                  {t('evolution.runTest')}
                </button>
              </div>

              {/* Test result */}
              {testResult && (
                <div className="space-y-3">
                  <div className={`rounded-lg p-3 text-sm border ${
                    testResult.error || testResult.status === 'failed'
                      ? 'border-red-500/30 bg-red-500/5'
                      : 'border-cyber/30 bg-cyber/5'
                  }`}>
                    {/* Status header */}
                    <div className="flex items-center gap-2 mb-2">
                      {testResult.status === 'completed' ? (
                        <CheckCircle size={14} className="text-cyber" />
                      ) : (
                        <XCircle size={14} className="text-red-400" />
                      )}
                      <span className={`font-medium text-xs ${
                        testResult.status === 'completed' ? 'text-cyber' : 'text-red-400'
                      }`}>
                        {testResult.status === 'completed' ? 'Test Completed' : 'Test Failed'}
                      </span>
                      {testResult.duration_ms != null && (
                        <span className="text-[10px] text-muted-foreground ml-auto">
                          {testResult.duration_ms}ms
                        </span>
                      )}
                    </div>
                    {/* Result content */}
                    {testResult.result ? (
                      <pre className="whitespace-pre-wrap font-mono text-xs text-foreground max-h-64 overflow-y-auto">
                        {testResult.result}
                      </pre>
                    ) : testResult.error ? (
                      <pre className="whitespace-pre-wrap font-mono text-xs text-red-400">
                        {testResult.error}
                      </pre>
                    ) : (
                      <pre className="whitespace-pre-wrap font-mono text-xs text-muted-foreground">
                        {JSON.stringify(testResult, null, 2)}
                      </pre>
                    )}
                  </div>

                  {/* Satisfaction buttons */}
                  {!showEvolveForm && (
                    <div className="flex items-center gap-3 p-3 rounded-lg border border-border bg-muted/30">
                      <span className="text-xs text-muted-foreground flex-1">{t('evolution.testAndEvolveDesc')}</span>
                      <button
                        onClick={() => { setTestResult(null); setShowEvolveForm(false); }}
                        className="flex items-center gap-1.5 px-3 py-1.5 text-[11px] font-medium rounded-lg bg-cyber/10 text-cyber border border-cyber/30 hover:bg-cyber/20 transition-colors"
                      >
                        <ThumbsUp size={11} />
                        {t('evolution.testResultSatisfied')}
                      </button>
                      <button
                        onClick={() => setShowEvolveForm(true)}
                        className="flex items-center gap-1.5 px-3 py-1.5 text-[11px] font-medium rounded-lg bg-rust/10 text-rust border border-rust/30 hover:bg-rust/20 transition-colors"
                      >
                        <ThumbsDown size={11} />
                        {t('evolution.testResultUnsatisfied')}
                      </button>
                    </div>
                  )}

                  {/* Evolution form (shown when unsatisfied) */}
                  {showEvolveForm && (
                    <div className="border border-rust/30 rounded-lg p-4 bg-rust/5 space-y-3">
                      <div className="flex items-center gap-2">
                        <ArrowUpCircle size={14} className="text-rust" />
                        <h4 className="text-sm font-semibold text-rust">{t('evolution.evolveFromTest')}</h4>
                      </div>
                      <p className="text-xs text-muted-foreground">{t('evolution.evolveDesc')}</p>
                      <textarea
                        value={evolveDesc}
                        onChange={e => setEvolveDesc(e.target.value)}
                        placeholder={t('evolution.evolveDescPlaceholder')}
                        rows={4}
                        className="w-full px-3 py-2 text-sm rounded-lg border border-border bg-background focus:outline-none focus:ring-1 focus:ring-rust resize-none"
                      />
                      <div className="flex justify-end gap-2">
                        <button
                          onClick={() => { setShowEvolveForm(false); setEvolveDesc(''); }}
                          className="px-3 py-1 text-xs rounded-lg border border-border hover:bg-accent"
                        >
                          {t('common.cancel')}
                        </button>
                        <button
                          onClick={handleEvolveFromTest}
                          disabled={triggeringEvolve || !evolveDesc.trim()}
                          className="flex items-center gap-1.5 px-4 py-1.5 text-xs font-medium rounded-lg bg-rust text-white hover:bg-rust/90 disabled:opacity-50 transition-colors"
                        >
                          {triggeringEvolve ? <Loader2 size={12} className="animate-spin" /> : <ArrowUpCircle size={12} />}
                          {t('evolution.evolveFromTest')}
                        </button>
                      </div>
                    </div>
                  )}
                </div>
              )}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

// ── Sub-components ──

function OverviewTab({
  summary, skillRecords, capRecords, t,
}: {
  summary: EvolutionSummary | null;
  skillRecords: EvolutionRecord[];
  capRecords: CoreEvolutionRecord[];
  t: (key: string, params?: Record<string, string | number>) => string;
}) {
  const se = summary?.skill_evolution || { total: 0, active: 0, completed: 0, failed: 0 };
  const ce = summary?.tool_evolution || { total: 0, active: 0, completed: 0, failed: 0 };
  const inv = summary?.inventory || { user_skills: 0, builtin_skills: 0, registered_tools: 0 };

  // Recent activity: merge both record types, sort by updated_at, take top 8
  const recentActivity = [
    ...skillRecords.slice(0, 20).map(r => ({ kind: 'skill' as const, name: r.skill_name, status: r.status, time: r.updated_at, id: r.id })),
    ...capRecords.slice(0, 20).map(r => ({ kind: 'tool_evolution' as const, name: r.capability_id, status: r.status, time: r.updated_at, id: r.id })),
  ].sort((a, b) => (b.time || 0) - (a.time || 0)).slice(0, 8);

  return (
    <div className="space-y-6">
      {/* Architecture diagram */}
      <div className="border border-border rounded-xl p-5 bg-card">
        <h3 className="text-xs font-semibold text-muted-foreground uppercase tracking-wider mb-4">
          {t('evolution.architecture')}
        </h3>
        <div className="space-y-1.5 font-mono text-xs">
          <ArchLayer
            label={t('evolution.layerAgent')}
            desc="LLM + IntentClassifier"
            color="text-rust"
            borderColor="border-rust/40"
            bgColor="bg-rust/5"
          />
          <div className="flex justify-center text-muted-foreground">▼</div>
          <ArchLayer
            label={t('evolution.layerSkill')}
            desc={`SKILL.rhai + SKILL.md — ${inv.user_skills + inv.builtin_skills} ${t('evolution.registered')}`}
            color="text-cyber"
            borderColor="border-cyber/40"
            bgColor="bg-cyber/5"
            badge={se.active > 0 ? `${se.active} ${t('evolution.evolving').toLowerCase()}` : undefined}
          />
          <div className="flex justify-center text-muted-foreground">▼</div>
          <ArchLayer
            label={t('evolution.layerTool')}
            desc={`${inv.registered_tools} ${t('evolution.builtinTools')}`}
            color="text-blue-400"
            borderColor="border-blue-400/40"
            bgColor="bg-blue-400/5"
          />
          <div className="flex justify-center text-muted-foreground">▼</div>
          <ArchLayer
            label={t('evolution.layerCapability')}
            desc={`bash / python / rust — ${t('evolution.dynamicTools')}`}
            color="text-purple-400"
            borderColor="border-purple-400/40"
            bgColor="bg-purple-400/5"
            badge={ce.active > 0 ? `${ce.active} ${t('evolution.evolving').toLowerCase()}` : undefined}
          />
        </div>
      </div>

      {/* Two-system comparison */}
      <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
        {/* Skill Evolution */}
        <div className="border border-border rounded-xl p-4 bg-card">
          <div className="flex items-center gap-2 mb-3">
            <Dna size={14} className="text-cyber" />
            <h3 className="text-sm font-semibold">{t('evolution.skillEvolution')}</h3>
          </div>
          <p className="text-[10px] text-muted-foreground mb-3">{t('evolution.skillDesc')}</p>
          <div className="grid grid-cols-3 gap-2 mb-3">
            <div className="text-center p-2 rounded-lg bg-muted/30">
              <p className="text-lg font-bold text-cyber">{se.completed}</p>
              <p className="text-[9px] text-muted-foreground uppercase">{t('evolution.completed')}</p>
            </div>
            <div className="text-center p-2 rounded-lg bg-muted/30">
              <p className="text-lg font-bold text-yellow-400">{se.active}</p>
              <p className="text-[9px] text-muted-foreground uppercase">{t('evolution.evolving')}</p>
            </div>
            <div className="text-center p-2 rounded-lg bg-muted/30">
              <p className="text-lg font-bold text-red-400">{se.failed}</p>
              <p className="text-[9px] text-muted-foreground uppercase">{t('evolution.failed')}</p>
            </div>
          </div>
          <div className="text-[10px] text-muted-foreground space-y-0.5">
            <div className="flex items-center gap-1.5">
              <span className="w-1.5 h-1.5 rounded-full bg-cyber inline-block" />
              {t('evolution.skillPipeline')}
            </div>
            <div className="flex items-center gap-1.5">
              <span className="w-1.5 h-1.5 rounded-full bg-muted-foreground inline-block" />
              {t('evolution.skillProduct')}
            </div>
          </div>
        </div>

        {/* Tool Evolution */}
        <div className="border border-border rounded-xl p-4 bg-card">
          <div className="flex items-center gap-2 mb-3">
            <Cpu size={14} className="text-purple-400" />
            <h3 className="text-sm font-semibold">{t('evolution.capEvolution')}</h3>
          </div>
          <p className="text-[10px] text-muted-foreground mb-3">{t('evolution.capDesc')}</p>
          <div className="grid grid-cols-3 gap-2 mb-3">
            <div className="text-center p-2 rounded-lg bg-muted/30">
              <p className="text-lg font-bold text-cyber">{ce.completed}</p>
              <p className="text-[9px] text-muted-foreground uppercase">{t('evolution.completed')}</p>
            </div>
            <div className="text-center p-2 rounded-lg bg-muted/30">
              <p className="text-lg font-bold text-yellow-400">{ce.active}</p>
              <p className="text-[9px] text-muted-foreground uppercase">{t('evolution.evolving')}</p>
            </div>
            <div className="text-center p-2 rounded-lg bg-muted/30">
              <p className="text-lg font-bold text-red-400">{ce.failed}</p>
              <p className="text-[9px] text-muted-foreground uppercase">{t('evolution.failed')}</p>
            </div>
          </div>
          <div className="text-[10px] text-muted-foreground space-y-0.5">
            <div className="flex items-center gap-1.5">
              <span className="w-1.5 h-1.5 rounded-full bg-purple-400 inline-block" />
              {t('evolution.capPipeline')}
            </div>
            <div className="flex items-center gap-1.5">
              <span className="w-1.5 h-1.5 rounded-full bg-muted-foreground inline-block" />
              {t('evolution.capProduct')}
            </div>
          </div>
        </div>
      </div>

      {/* Recent activity */}
      {recentActivity.length > 0 && (
        <div className="border border-border rounded-xl p-4 bg-card">
          <h3 className="text-xs font-semibold text-muted-foreground uppercase tracking-wider mb-3">
            {t('evolution.recentActivity')}
          </h3>
          <div className="space-y-1.5">
            {recentActivity.map((item, i) => (
              <div key={i} className="flex items-center gap-2.5 text-xs py-1">
                {statusIcon(item.status)}
                <span className={`text-[10px] px-1.5 py-0.5 rounded font-medium ${
                  item.kind === 'skill' ? 'bg-cyber/10 text-cyber' : 'bg-purple-400/10 text-purple-400'
                }`}>
                  {item.kind === 'skill' ? 'SKILL' : 'CAP'}
                </span>
                <span className="font-medium flex-1 truncate">{item.name}</span>
                <span className={`text-[10px] font-bold uppercase tracking-wider ${statusColor(item.status)}`}>
                  {item.status}
                </span>
                <span className="text-[10px] text-muted-foreground w-16 text-right">{formatTime(item.time)}</span>
              </div>
            ))}
          </div>
        </div>
      )}

      {/* Decision boundary */}
      <div className="border border-border rounded-xl p-4 bg-card">
        <h3 className="text-xs font-semibold text-muted-foreground uppercase tracking-wider mb-3">
          {t('evolution.decisionBoundary')}
        </h3>
        <div className="text-xs text-muted-foreground space-y-2">
          <div className="flex items-start gap-2">
            <span className="text-cyber font-bold shrink-0">Skill →</span>
            <span>{t('evolution.decisionSkill')}</span>
          </div>
          <div className="flex items-start gap-2">
            <span className="text-purple-400 font-bold shrink-0">Cap →</span>
            <span>{t('evolution.decisionCap')}</span>
          </div>
          <div className="flex items-start gap-2">
            <span className="text-yellow-400 font-bold shrink-0">Auto →</span>
            <span>{t('evolution.decisionAuto')}</span>
          </div>
        </div>
      </div>
    </div>
  );
}

function ArchLayer({ label, desc, color, borderColor, bgColor, badge }: {
  label: string; desc: string; color: string; borderColor: string; bgColor: string; badge?: string;
}) {
  return (
    <div className={`border ${borderColor} rounded-lg p-2.5 ${bgColor} flex items-center justify-between`}>
      <div>
        <span className={`font-bold ${color}`}>{label}</span>
        <span className="text-muted-foreground ml-2">{desc}</span>
      </div>
      {badge && (
        <span className="text-[9px] px-1.5 py-0.5 rounded-full bg-yellow-400/15 text-yellow-400 font-medium animate-pulse">
          {badge}
        </span>
      )}
    </div>
  );
}

function MiniStat({ label, value, icon, color }: { label: string; value: string; icon: React.ReactNode; color?: string }) {
  return (
    <div className="border border-border rounded-lg p-3 bg-card">
      <div className="flex items-center gap-2">
        <div className={color || 'text-muted-foreground'}>{icon}</div>
        <div>
          <p className="text-[10px] text-muted-foreground uppercase tracking-wider">{label}</p>
          <p className={`text-sm font-bold ${color || ''}`}>{value}</p>
        </div>
      </div>
    </div>
  );
}

function EmptyState({ message }: { message: string }) {
  return (
    <div className="flex flex-col items-center justify-center py-12 text-muted-foreground">
      <Dna size={32} className="mb-3 opacity-30" />
      <p className="text-sm">{message}</p>
    </div>
  );
}

function SkillRecordCard({
  record, expanded, onToggle, onDelete, t,
}: {
  record: EvolutionRecord;
  expanded: boolean;
  onToggle: () => void;
  onDelete: () => void;
  t: (key: string, params?: Record<string, string | number>) => string;
}) {
  const [showCode, setShowCode] = useState(false);
  const [deleteConfirm, setDeleteConfirm] = useState(false);

  return (
    <div className="border border-border rounded-xl bg-card overflow-hidden transition-all">
      {/* Header row */}
      <div
        className="flex items-center gap-3 px-4 py-3 cursor-pointer hover:bg-accent/30 transition-colors"
        onClick={onToggle}
      >
        {expanded ? <ChevronDown size={14} className="text-muted-foreground shrink-0" /> : <ChevronRight size={14} className="text-muted-foreground shrink-0" />}
        {statusIcon(record.status)}
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2">
            <span className="font-medium text-sm truncate">{record.skill_name}</span>
            <span className={`text-[10px] font-bold uppercase tracking-wider ${statusColor(record.status)}`}>
              {record.status}
            </span>
            {record.attempt > 1 && (
              <span className="text-[10px] px-1.5 py-0.5 rounded bg-muted text-muted-foreground">
                {t('evolution.attempt', { n: record.attempt })}
              </span>
            )}
          </div>
          <div className="flex items-center gap-3 mt-0.5">
            <span className="text-[10px] text-muted-foreground">
              {triggerLabel(record.context?.trigger)}
            </span>
            <span className="text-[10px] text-muted-foreground">
              {formatTime(record.updated_at)}
            </span>
            <PipelineStages status={record.status} stages={SKILL_STAGES} />
          </div>
        </div>
        <button
          onClick={e => { e.stopPropagation(); setDeleteConfirm(true); }}
          className="p-1.5 rounded-md hover:bg-destructive/20 text-muted-foreground hover:text-destructive opacity-0 group-hover:opacity-100 transition-opacity shrink-0"
        >
          <Trash2 size={12} />
        </button>
      </div>

      {/* Expanded detail */}
      {expanded && (
        <div className="border-t border-border px-4 py-3 space-y-3 text-xs">
          {/* Trigger info */}
          <DetailSection title={t('evolution.triggerInfo')}>
            <div className="grid grid-cols-2 gap-2">
              <KV label={t('evolution.id')} value={record.id} />
              <KV label={t('evolution.version')} value={record.context?.current_version || '—'} />
              <KV label={t('evolution.created')} value={formatTime(record.created_at)} />
              <KV label={t('evolution.updated')} value={formatTime(record.updated_at)} />
            </div>
            {record.context?.error_stack && (
              <div className="mt-2">
                <span className="text-muted-foreground">{t('evolution.errorStack')}:</span>
                <pre className="mt-1 p-2 rounded bg-muted/50 text-[10px] font-mono whitespace-pre-wrap max-h-32 overflow-y-auto">
                  {record.context.error_stack}
                </pre>
              </div>
            )}
          </DetailSection>

          {/* Patch */}
          {record.patch && (
            <DetailSection title={t('evolution.patch')}>
              <p className="text-muted-foreground mb-1">{record.patch.explanation}</p>
              <button
                onClick={() => setShowCode(!showCode)}
                className="flex items-center gap-1 text-[10px] text-rust hover:underline"
              >
                {showCode ? <EyeOff size={10} /> : <Eye size={10} />}
                {showCode ? t('evolution.hideCode') : t('evolution.showCode')}
              </button>
              {showCode && (
                <pre className="mt-1 p-2 rounded bg-muted/50 text-[10px] font-mono whitespace-pre-wrap max-h-48 overflow-y-auto">
                  {record.patch.diff}
                </pre>
              )}
            </DetailSection>
          )}

          {/* Audit */}
          {record.audit && (
            <DetailSection title={t('evolution.audit')}>
              <div className="flex items-center gap-2 mb-1">
                {record.audit.passed
                  ? <CheckCircle size={12} className="text-cyber" />
                  : <XCircle size={12} className="text-red-400" />}
                <span className={record.audit.passed ? 'text-cyber' : 'text-red-400'}>
                  {record.audit.passed ? t('evolution.auditPassed') : t('evolution.auditFailed')}
                </span>
              </div>
              {record.audit.issues?.length > 0 && (
                <div className="space-y-1">
                  {record.audit.issues.map((issue, i) => (
                    <div key={i} className="flex items-start gap-1.5">
                      <AlertTriangle size={10} className={
                        issue.severity === 'critical' ? 'text-red-400 mt-0.5' :
                        issue.severity === 'warning' ? 'text-yellow-400 mt-0.5' : 'text-muted-foreground mt-0.5'
                      } />
                      <span className="text-muted-foreground">
                        <span className="font-medium">[{issue.category}]</span> {issue.message}
                      </span>
                    </div>
                  ))}
                </div>
              )}
            </DetailSection>
          )}

          {/* Shadow test */}
          {record.shadow_test && (
            <DetailSection title={t('evolution.shadowTest')}>
              <div className="flex items-center gap-2">
                {record.shadow_test.passed
                  ? <CheckCircle size={12} className="text-cyber" />
                  : <XCircle size={12} className="text-red-400" />}
                <span className={record.shadow_test.passed ? 'text-cyber' : 'text-red-400'}>
                  {record.shadow_test.passed ? t('evolution.testPassed') : t('evolution.testFailed')}
                </span>
              </div>
            </DetailSection>
          )}

          {/* Rollout */}
          {record.rollout && (
            <DetailSection title={t('evolution.rollout')}>
              <div className="flex items-center gap-2 mb-1">
                <span className="text-muted-foreground">{t('evolution.stage')}:</span>
                <span className="font-medium">{record.rollout.current_stage + 1} / {record.rollout.stages.length}</span>
              </div>
              <div className="flex gap-1">
                {record.rollout.stages.map((stage, i) => (
                  <div
                    key={i}
                    className={`flex-1 h-1.5 rounded-full ${
                      i <= record.rollout!.current_stage ? 'bg-cyber' : 'bg-muted'
                    }`}
                    title={`${stage.percentage}% — ${stage.duration_hours}h`}
                  />
                ))}
              </div>
            </DetailSection>
          )}

          {/* Feedback history */}
          {record.feedback_history?.length > 0 && (
            <DetailSection title={t('evolution.feedbackHistory')}>
              <div className="space-y-2">
                {record.feedback_history.map((fb, i) => (
                  <div key={i} className="border border-border rounded-lg p-2 bg-muted/30">
                    <div className="flex items-center gap-2 mb-1">
                      <RotateCcw size={10} className="text-muted-foreground" />
                      <span className="font-medium">{t('evolution.attempt', { n: fb.attempt })}</span>
                      <span className="text-muted-foreground">— {fb.stage}</span>
                      <span className="text-muted-foreground ml-auto">{formatTime(fb.timestamp)}</span>
                    </div>
                    <p className="text-muted-foreground">{fb.feedback}</p>
                  </div>
                ))}
              </div>
            </DetailSection>
          )}
        </div>
      )}

      {/* Delete confirmation */}
      {deleteConfirm && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50" onClick={() => setDeleteConfirm(false)}>
          <div className="bg-card border border-border rounded-xl p-6 max-w-sm w-full mx-4 shadow-xl" onClick={e => e.stopPropagation()}>
            <div className="flex items-center gap-3 mb-4">
              <div className="p-2 rounded-full bg-destructive/10">
                <Trash2 size={20} className="text-destructive" />
              </div>
              <h3 className="font-semibold">{t('evolution.deleteRecord')}</h3>
            </div>
            <p className="text-sm text-muted-foreground mb-1">{t('evolution.deleteConfirm')}</p>
            <p className="text-sm font-medium mb-6 truncate">{record.skill_name} — {record.id}</p>
            <div className="flex justify-end gap-2">
              <button onClick={() => setDeleteConfirm(false)} className="px-4 py-1.5 text-sm rounded-lg border border-border hover:bg-accent">
                {t('common.cancel')}
              </button>
              <button
                onClick={() => { setDeleteConfirm(false); onDelete(); }}
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

function CapRecordCard({
  record, expanded, onToggle, onDelete, t,
}: {
  record: CoreEvolutionRecord;
  expanded: boolean;
  onToggle: () => void;
  onDelete: () => void;
  t: (key: string, params?: Record<string, string | number>) => string;
}) {
  const [deleteConfirm, setDeleteConfirm] = useState(false);
  const [showCode, setShowCode] = useState(false);
  const [versions, setVersions] = useState<any[]>([]);
  const [showVersions, setShowVersions] = useState(false);
  const [showSchema, setShowSchema] = useState(false);

  useEffect(() => {
    if (expanded && showVersions && versions.length === 0) {
      getToolEvolutionVersions(record.capability_id).then(res => {
        setVersions(res.versions || []);
      }).catch(() => {});
    }
  }, [expanded, showVersions, record.capability_id]);

  const providerBadgeColor = {
    Process: 'bg-blue-400/10 text-blue-400',
    RhaiScript: 'bg-cyber/10 text-cyber',
    ExternalApi: 'bg-purple-400/10 text-purple-400',
    DynamicLibrary: 'bg-orange-400/10 text-orange-400',
    BuiltIn: 'bg-muted text-muted-foreground',
  }[record.provider_kind] || 'bg-muted text-muted-foreground';

  return (
    <div className="border border-border rounded-xl bg-card overflow-hidden transition-all">
      <div
        className="flex items-center gap-3 px-4 py-3 cursor-pointer hover:bg-accent/30 transition-colors"
        onClick={onToggle}
      >
        {expanded ? <ChevronDown size={14} className="text-muted-foreground shrink-0" /> : <ChevronRight size={14} className="text-muted-foreground shrink-0" />}
        {statusIcon(record.status)}
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2">
            <Cpu size={12} className="text-purple-400" />
            <span className="font-medium text-sm truncate">{record.capability_id}</span>
            <span className={`text-[10px] font-bold uppercase tracking-wider ${statusColor(record.status)}`}>
              {record.status}
            </span>
            <span className={`text-[10px] px-1.5 py-0.5 rounded font-medium ${providerBadgeColor}`}>
              {record.provider_kind}
            </span>
            {record.attempt > 1 && (
              <span className="text-[10px] px-1.5 py-0.5 rounded bg-muted text-muted-foreground">
                {t('evolution.attempt', { n: record.attempt })}
              </span>
            )}
          </div>
          <div className="flex items-center gap-3 mt-0.5">
            <span className="text-[10px] text-muted-foreground truncate max-w-[300px]">
              {record.description}
            </span>
            <span className="text-[10px] text-muted-foreground">
              {formatTime(record.updated_at)}
            </span>
            <PipelineStages status={record.status} stages={CAP_STAGES} />
          </div>
        </div>
        <button
          onClick={e => { e.stopPropagation(); setDeleteConfirm(true); }}
          className="p-1.5 rounded-md hover:bg-destructive/20 text-muted-foreground hover:text-destructive shrink-0"
        >
          <Trash2 size={12} />
        </button>
      </div>

      {expanded && (
        <div className="border-t border-border px-4 py-3 space-y-3 text-xs">
          <DetailSection title={t('evolution.detail')}>
            <div className="grid grid-cols-2 gap-2">
              <KV label={t('evolution.id')} value={record.id} />
              <KV label={t('evolution.provider')} value={record.provider_kind} />
              <KV label={t('evolution.created')} value={formatTime(record.created_at)} />
              <KV label={t('evolution.updated')} value={formatTime(record.updated_at)} />
            </div>
            <div className="mt-2">
              <span className="text-muted-foreground">{t('evolution.description')}:</span>
              <p className="mt-1 text-foreground">{record.description}</p>
            </div>
            {record.artifact_path && (
              <div className="mt-1">
                <KV label={t('evolution.artifactPath')} value={record.artifact_path} />
              </div>
            )}
          </DetailSection>

          {/* Input/Output Schema */}
          {(record.input_schema || record.output_schema) && (
            <DetailSection title={t('evolution.schemas')}>
              <button
                onClick={() => setShowSchema(!showSchema)}
                className="flex items-center gap-1 text-[10px] text-purple-400 hover:underline mb-1"
              >
                <FileCode size={10} />
                {showSchema ? t('evolution.hideCode') : t('evolution.showCode')}
              </button>
              {showSchema && (
                <div className="space-y-2">
                  {record.input_schema && (
                    <div>
                      <span className="text-muted-foreground font-medium">Input:</span>
                      <pre className="mt-0.5 p-2 rounded bg-muted/50 text-[10px] font-mono whitespace-pre-wrap max-h-32 overflow-y-auto">
                        {JSON.stringify(record.input_schema, null, 2)}
                      </pre>
                    </div>
                  )}
                  {record.output_schema && (
                    <div>
                      <span className="text-muted-foreground font-medium">Output:</span>
                      <pre className="mt-0.5 p-2 rounded bg-muted/50 text-[10px] font-mono whitespace-pre-wrap max-h-32 overflow-y-auto">
                        {JSON.stringify(record.output_schema, null, 2)}
                      </pre>
                    </div>
                  )}
                </div>
              )}
            </DetailSection>
          )}

          {record.source_code && (
            <DetailSection title={t('evolution.sourceCode')}>
              <button
                onClick={() => setShowCode(!showCode)}
                className="flex items-center gap-1 text-[10px] text-rust hover:underline mb-1"
              >
                {showCode ? <EyeOff size={10} /> : <Eye size={10} />}
                {showCode ? t('evolution.hideCode') : t('evolution.showCode')}
              </button>
              {showCode && (
                <pre className="p-2 rounded bg-muted/50 text-[10px] font-mono whitespace-pre-wrap max-h-48 overflow-y-auto">
                  {record.source_code}
                </pre>
              )}
            </DetailSection>
          )}

          {record.compile_output && (
            <DetailSection title={t('evolution.compileOutput')}>
              <pre className="p-2 rounded bg-muted/50 text-[10px] font-mono whitespace-pre-wrap max-h-32 overflow-y-auto">
                {record.compile_output}
              </pre>
            </DetailSection>
          )}

          {record.validation && (
            <DetailSection title={t('evolution.validation')}>
              <div className="flex items-center gap-2 mb-1">
                {record.validation.passed
                  ? <CheckCircle size={12} className="text-cyber" />
                  : <XCircle size={12} className="text-red-400" />}
                <span className={record.validation.passed ? 'text-cyber' : 'text-red-400'}>
                  {record.validation.passed ? t('evolution.validationPassed') : t('evolution.validationFailed')}
                </span>
              </div>
              {record.validation.checks?.map((check, i) => (
                <div key={i} className="flex items-center gap-1.5 ml-4">
                  {check.passed ? <CheckCircle size={10} className="text-cyber" /> : <XCircle size={10} className="text-red-400" />}
                  <span className="text-muted-foreground">{check.name}: {check.message}</span>
                </div>
              ))}
            </DetailSection>
          )}

          {/* Version History */}
          <DetailSection title={t('evolution.versionHistory')}>
            <button
              onClick={() => setShowVersions(!showVersions)}
              className="flex items-center gap-1 text-[10px] text-blue-400 hover:underline mb-1"
            >
              <History size={10} />
              {showVersions ? t('evolution.hideVersions') : t('evolution.showVersions')}
            </button>
            {showVersions && (
              <div className="space-y-1 mt-1">
                {versions.length === 0 ? (
                  <span className="text-muted-foreground">{t('evolution.noVersions')}</span>
                ) : (
                  versions.map((v, i) => (
                    <div key={i} className="flex items-center gap-2 py-1 border-b border-border/50 last:border-0">
                      <Tag size={10} className="text-blue-400 shrink-0" />
                      <span className="font-mono font-medium">{v.version}</span>
                      <span className="text-[9px] px-1 py-0.5 rounded bg-muted text-muted-foreground">{v.source}</span>
                      <span className="text-muted-foreground flex-1 truncate text-[10px]">{v.changelog || '—'}</span>
                      <span className="text-[10px] text-muted-foreground">{formatTime(v.created_at)}</span>
                    </div>
                  ))
                )}
              </div>
            )}
          </DetailSection>

          {record.feedback_history?.length > 0 && (
            <DetailSection title={t('evolution.feedbackHistory')}>
              <div className="space-y-2">
                {record.feedback_history.map((fb, i) => (
                  <div key={i} className="border border-border rounded-lg p-2 bg-muted/30">
                    <div className="flex items-center gap-2 mb-1">
                      <RotateCcw size={10} className="text-muted-foreground" />
                      <span className="font-medium">{t('evolution.attempt', { n: fb.attempt })}</span>
                      <span className="text-muted-foreground">— {fb.stage}</span>
                    </div>
                    <p className="text-muted-foreground">{fb.feedback}</p>
                  </div>
                ))}
              </div>
            </DetailSection>
          )}
        </div>
      )}

      {deleteConfirm && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50" onClick={() => setDeleteConfirm(false)}>
          <div className="bg-card border border-border rounded-xl p-6 max-w-sm w-full mx-4 shadow-xl" onClick={e => e.stopPropagation()}>
            <div className="flex items-center gap-3 mb-4">
              <div className="p-2 rounded-full bg-destructive/10">
                <Trash2 size={20} className="text-destructive" />
              </div>
              <h3 className="font-semibold">{t('evolution.deleteRecord')}</h3>
            </div>
            <p className="text-sm text-muted-foreground mb-6">{t('evolution.deleteConfirm')}</p>
            <div className="flex justify-end gap-2">
              <button onClick={() => setDeleteConfirm(false)} className="px-4 py-1.5 text-sm rounded-lg border border-border hover:bg-accent">
                {t('common.cancel')}
              </button>
              <button
                onClick={() => { setDeleteConfirm(false); onDelete(); }}
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

function DetailSection({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div>
      <h4 className="text-[10px] font-semibold text-muted-foreground uppercase tracking-wider mb-1.5">{title}</h4>
      {children}
    </div>
  );
}

function KV({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <span className="text-muted-foreground">{label}:</span>
      <span className="ml-1 font-mono text-[10px]">{value}</span>
    </div>
  );
}
