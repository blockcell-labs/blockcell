import { useEffect, useState } from 'react';
import {
  Bell, Plus, Trash2, RefreshCw, Loader2, AlertTriangle, Check,
  ChevronDown, ChevronRight, Power, PowerOff, History, Edit2, X,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import {
  getAlerts, createAlert, updateAlert, deleteAlert, getAlertHistory,
  type AlertRule, type AlertHistoryEntry,
} from '@/lib/api';
import { useT } from '@/lib/i18n';

const OPERATORS = [
  { value: 'gt', label: '> Greater than' },
  { value: 'lt', label: '< Less than' },
  { value: 'gte', label: '>= Greater or equal' },
  { value: 'lte', label: '<= Less or equal' },
  { value: 'eq', label: '= Equal' },
  { value: 'ne', label: '!= Not equal' },
  { value: 'change_pct', label: '% Change percent' },
  { value: 'cross_above', label: '↑ Cross above' },
  { value: 'cross_below', label: '↓ Cross below' },
];

export function AlertsPage() {
  const t = useT();
  const [rules, setRules] = useState<AlertRule[]>([]);
  const [history, setHistory] = useState<AlertHistoryEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [showCreate, setShowCreate] = useState(false);
  const [activeTab, setActiveTab] = useState<'rules' | 'history'>('rules');
  const [expandedId, setExpandedId] = useState<string | null>(null);

  const [form, setForm] = useState({
    name: '',
    source: '{"tool": "finance_api", "params": {"action": "stock_quote", "symbol": ""}}',
    metric_path: 'price',
    operator: 'gt',
    threshold: '',
    cooldown_secs: '300',
    check_interval_secs: '60',
    notify_channel: 'desktop',
  });

  useEffect(() => {
    fetchAll();
  }, []);

  async function fetchAll() {
    setLoading(true);
    try {
      const [alertsData, historyData] = await Promise.allSettled([getAlerts(), getAlertHistory()]);
      if (alertsData.status === 'fulfilled') setRules(alertsData.value.rules || []);
      if (historyData.status === 'fulfilled') setHistory(historyData.value.history || []);
    } finally {
      setLoading(false);
    }
  }

  async function handleCreate() {
    try {
      let source;
      try {
        source = JSON.parse(form.source);
      } catch {
        source = { tool: 'finance_api', params: { action: 'stock_quote', symbol: form.source } };
      }

      await createAlert({
        name: form.name,
        source,
        metric_path: form.metric_path,
        operator: form.operator,
        threshold: parseFloat(form.threshold),
        cooldown_secs: parseInt(form.cooldown_secs) || 300,
        check_interval_secs: parseInt(form.check_interval_secs) || 60,
        notify: { channel: form.notify_channel },
      });
      setShowCreate(false);
      setForm({ name: '', source: '', metric_path: 'price', operator: 'gt', threshold: '', cooldown_secs: '300', check_interval_secs: '60', notify_channel: 'desktop' });
      fetchAll();
    } catch {
      // ignore
    }
  }

  async function handleToggle(rule: AlertRule) {
    try {
      await updateAlert(rule.id, { enabled: !rule.enabled } as any);
      setRules(rules.map((r) => r.id === rule.id ? { ...r, enabled: !r.enabled } : r));
    } catch {
      // ignore
    }
  }

  async function handleDelete(id: string) {
    try {
      await deleteAlert(id);
      setRules(rules.filter((r) => r.id !== id));
    } catch {
      // ignore
    }
  }

  function formatTime(ms?: number) {
    if (!ms) return '—';
    return new Date(ms).toLocaleString();
  }

  function operatorLabel(op: string) {
    return OPERATORS.find((o) => o.value === op)?.label || op;
  }

  const enabledCount = rules.filter((r) => r.enabled).length;
  const triggeredCount = rules.filter((r) => (r.state?.trigger_count || 0) > 0).length;

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="border-b border-border px-6 py-4 flex items-center justify-between">
        <div>
          <h1 className="text-lg font-semibold">{t('alerts.title')}</h1>
          <p className="text-sm text-muted-foreground">
            {rules.length} rules · {enabledCount} active · {triggeredCount} triggered
          </p>
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={() => setShowCreate(!showCreate)}
            className="flex items-center gap-1.5 px-3 py-1.5 text-sm rounded-lg bg-primary text-primary-foreground hover:bg-primary/90"
          >
            <Plus size={14} /> {t('alerts.createRule')}
          </button>
          <button
            onClick={fetchAll}
            className="p-2 rounded-lg hover:bg-accent text-muted-foreground"
          >
            <RefreshCw size={16} className={loading ? 'animate-spin' : ''} />
          </button>
        </div>
      </div>

      {/* Tabs */}
      <div className="border-b border-border px-6 flex gap-4">
        <button
          onClick={() => setActiveTab('rules')}
          className={cn(
            'py-2.5 text-sm font-medium border-b-2 transition-colors',
            activeTab === 'rules'
              ? 'border-rust text-rust'
              : 'border-transparent text-muted-foreground hover:text-foreground'
          )}
        >
          <Bell size={14} className="inline mr-1.5" />
          Rules ({rules.length})
        </button>
        <button
          onClick={() => setActiveTab('history')}
          className={cn(
            'py-2.5 text-sm font-medium border-b-2 transition-colors',
            activeTab === 'history'
              ? 'border-rust text-rust'
              : 'border-transparent text-muted-foreground hover:text-foreground'
          )}
        >
          <History size={14} className="inline mr-1.5" />
          History ({history.length})
        </button>
      </div>

      {/* Create form */}
      {showCreate && (
        <div className="border-b border-border p-4 bg-card/50 space-y-3">
          <div className="grid grid-cols-2 gap-3">
            <input
              value={form.name}
              onChange={(e) => setForm({ ...form, name: e.target.value })}
              placeholder="Rule name (e.g. BTC > 100k)"
              className="px-3 py-1.5 text-sm bg-background border border-border rounded-lg outline-none focus:ring-1 focus:ring-ring"
            />
            <input
              value={form.metric_path}
              onChange={(e) => setForm({ ...form, metric_path: e.target.value })}
              placeholder="Metric path (e.g. price)"
              className="px-3 py-1.5 text-sm bg-background border border-border rounded-lg outline-none focus:ring-1 focus:ring-ring"
            />
          </div>
          <textarea
            value={form.source}
            onChange={(e) => setForm({ ...form, source: e.target.value })}
            placeholder='Data source JSON or symbol (e.g. AAPL or {"tool": "finance_api", "params": {...}})'
            rows={2}
            className="w-full px-3 py-1.5 text-sm bg-background border border-border rounded-lg outline-none focus:ring-1 focus:ring-ring resize-none font-mono"
          />
          <div className="flex items-center gap-3 flex-wrap">
            <select
              value={form.operator}
              onChange={(e) => setForm({ ...form, operator: e.target.value })}
              className="px-3 py-1.5 text-sm bg-background border border-border rounded-lg outline-none"
            >
              {OPERATORS.map((op) => (
                <option key={op.value} value={op.value}>{op.label}</option>
              ))}
            </select>
            <input
              value={form.threshold}
              onChange={(e) => setForm({ ...form, threshold: e.target.value })}
              placeholder="Threshold"
              type="number"
              step="any"
              className="px-3 py-1.5 text-sm bg-background border border-border rounded-lg outline-none w-32"
            />
            <input
              value={form.check_interval_secs}
              onChange={(e) => setForm({ ...form, check_interval_secs: e.target.value })}
              placeholder="Check interval (s)"
              type="number"
              className="px-3 py-1.5 text-sm bg-background border border-border rounded-lg outline-none w-36"
            />
            <select
              value={form.notify_channel}
              onChange={(e) => setForm({ ...form, notify_channel: e.target.value })}
              className="px-3 py-1.5 text-sm bg-background border border-border rounded-lg outline-none"
            >
              <option value="desktop">Desktop</option>
              <option value="message">Message</option>
              <option value="webhook">Webhook</option>
              <option value="email">Email</option>
            </select>
            <button
              onClick={handleCreate}
              disabled={!form.name || !form.threshold}
              className="px-4 py-1.5 text-sm rounded-lg bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
            >
              {t('common.create')}
            </button>
            <button
              onClick={() => setShowCreate(false)}
              className="p-1.5 rounded-lg hover:bg-accent text-muted-foreground"
            >
              <X size={16} />
            </button>
          </div>
        </div>
      )}

      {/* Content */}
      <div className="flex-1 overflow-y-auto p-6">
        {loading ? (
          <div className="flex items-center justify-center h-32">
            <Loader2 size={24} className="animate-spin text-muted-foreground" />
          </div>
        ) : activeTab === 'rules' ? (
          rules.length === 0 ? (
            <div className="flex flex-col items-center justify-center h-32 text-muted-foreground">
              <Bell size={32} className="mb-2 opacity-30" />
              <p className="text-sm">{t('alerts.empty')}</p>
              <p className="text-xs mt-1">{t('alerts.emptyHint')}</p>
            </div>
          ) : (
            <div className="space-y-2">
              {rules.map((rule) => (
                <div key={rule.id} className="group border border-border rounded-lg bg-card overflow-hidden">
                  <div
                    className="flex items-center gap-3 px-4 py-3 cursor-pointer hover:bg-accent/30 transition-colors"
                    onClick={() => setExpandedId(expandedId === rule.id ? null : rule.id)}
                  >
                    {expandedId === rule.id ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
                    <div className={cn('w-2 h-2 rounded-full', rule.enabled ? 'bg-cyber' : 'bg-muted-foreground')} />
                    <span className="font-medium text-sm flex-1">{rule.name}</span>
                    <span className="text-xs font-mono text-muted-foreground">
                      {operatorLabel(rule.operator)} {rule.threshold}
                    </span>
                    {(rule.state?.trigger_count || 0) > 0 && (
                      <span className="text-xs px-1.5 py-0.5 rounded-full bg-amber-500/10 text-amber-500">
                        {rule.state.trigger_count}× triggered
                      </span>
                    )}
                    <div className="flex items-center gap-1 opacity-0 group-hover:opacity-100 transition-opacity">
                      <button
                        onClick={(e) => { e.stopPropagation(); handleToggle(rule); }}
                        className="p-1 rounded hover:bg-accent"
                        title={rule.enabled ? 'Disable' : 'Enable'}
                      >
                        {rule.enabled ? <Power size={14} className="text-cyber" /> : <PowerOff size={14} className="text-muted-foreground" />}
                      </button>
                      <button
                        onClick={(e) => { e.stopPropagation(); handleDelete(rule.id); }}
                        className="p-1 rounded hover:bg-destructive/20 text-destructive"
                      >
                        <Trash2 size={14} />
                      </button>
                    </div>
                  </div>
                  {expandedId === rule.id && (
                    <div className="border-t border-border px-4 py-3 space-y-2 text-xs">
                      <div className="grid grid-cols-2 gap-x-6 gap-y-1.5">
                        <div><span className="text-muted-foreground">Metric path:</span> <span className="font-mono">{rule.metric_path}</span></div>
                        <div><span className="text-muted-foreground">Check interval:</span> {rule.check_interval_secs}s</div>
                        <div><span className="text-muted-foreground">Cooldown:</span> {rule.cooldown_secs}s</div>
                        <div><span className="text-muted-foreground">Notify:</span> {rule.notify?.channel || 'desktop'}</div>
                        <div><span className="text-muted-foreground">Last value:</span> {rule.state?.last_value ?? '—'}</div>
                        <div><span className="text-muted-foreground">Last check:</span> {formatTime(rule.state?.last_check_at)}</div>
                        <div><span className="text-muted-foreground">Last triggered:</span> {formatTime(rule.state?.last_triggered_at)}</div>
                        <div><span className="text-muted-foreground">Created:</span> {formatTime(rule.created_at)}</div>
                      </div>
                      <div>
                        <span className="text-muted-foreground">Source:</span>
                        <pre className="mt-1 bg-muted/50 rounded p-2 overflow-x-auto font-mono">
                          {JSON.stringify(rule.source, null, 2)}
                        </pre>
                      </div>
                      {rule.state?.last_error && (
                        <div className="text-red-500">
                          <AlertTriangle size={12} className="inline mr-1" />
                          {rule.state.last_error}
                        </div>
                      )}
                    </div>
                  )}
                </div>
              ))}
            </div>
          )
        ) : (
          /* History tab */
          history.length === 0 ? (
            <div className="flex flex-col items-center justify-center h-32 text-muted-foreground">
              <History size={32} className="mb-2 opacity-30" />
              <p className="text-sm">{t('alerts.noHistory')}</p>
            </div>
          ) : (
            <div className="space-y-2">
              {history.map((entry, i) => (
                <div key={i} className="border border-border rounded-lg p-4 bg-card flex items-center gap-3">
                  <AlertTriangle size={16} className="text-amber-500 shrink-0" />
                  <div className="flex-1 min-w-0">
                    <div className="flex items-center gap-2">
                      <span className="font-medium text-sm">{entry.name}</span>
                      <span className="text-xs text-muted-foreground font-mono">{entry.operator} {entry.threshold}</span>
                    </div>
                    <p className="text-xs text-muted-foreground mt-0.5">
                      Triggered {entry.trigger_count}× · Last value: {entry.last_value ?? '—'} · {formatTime(entry.last_triggered_at)}
                    </p>
                  </div>
                </div>
              ))}
            </div>
          )
        )}
      </div>
    </div>
  );
}
