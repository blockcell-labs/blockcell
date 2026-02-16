import { useEffect, useRef, useState } from 'react';
import { Ghost, Settings, Activity, RefreshCw, Clock, Brain, MessageSquare, Wrench } from 'lucide-react';
import { getGhostConfig, updateGhostConfig, getGhostActivity, getGhostModelOptions } from '@/lib/api';
import type { GhostConfig, GhostActivity, GhostModelOptions } from '@/lib/api';
import { cn } from '@/lib/utils';
import { useT } from '@/lib/i18n';

export function GhostPage() {
  const [tab, setTab] = useState<'config' | 'activity'>('config');
  const t = useT();

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="border-b border-border px-6 py-4 flex items-center justify-between shrink-0">
        <div className="flex items-center gap-3">
          <Ghost className="w-6 h-6 text-purple-500" />
          <h1 className="text-lg font-semibold">{t('ghost.title')}</h1>
        </div>
        <div className="flex gap-1 bg-muted rounded-lg p-1">
          <button
            onClick={() => setTab('config')}
            className={cn(
              'px-3 py-1.5 rounded-md text-sm font-medium transition-colors flex items-center gap-1.5',
              tab === 'config' ? 'bg-background shadow-sm text-foreground' : 'text-muted-foreground hover:text-foreground'
            )}
          >
            <Settings className="w-4 h-4" /> {t('ghost.settings')}
          </button>
          <button
            onClick={() => setTab('activity')}
            className={cn(
              'px-3 py-1.5 rounded-md text-sm font-medium transition-colors flex items-center gap-1.5',
              tab === 'activity' ? 'bg-background shadow-sm text-foreground' : 'text-muted-foreground hover:text-foreground'
            )}
          >
            <Activity className="w-4 h-4" /> {t('ghost.activityLog')}
          </button>
        </div>
      </div>

      {/* Content */}
      <div className="flex-1 overflow-y-auto p-6">
        {tab === 'config' ? <GhostConfigPanel /> : <GhostActivityPanel />}
      </div>
    </div>
  );
}

function GhostConfigPanel() {
  const [config, setConfig] = useState<GhostConfig | null>(null);
  const [modelOptions, setModelOptions] = useState<GhostModelOptions | null>(null);
  const [selectedProvider, setSelectedProvider] = useState<string>('');
  const [modelInput, setModelInput] = useState<string>('');
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [message, setMessage] = useState('');
  const [customMode, setCustomMode] = useState(false);
  const t = useT();

  const saveTimerRef = useRef<number | null>(null);
  const lastSavedRef = useRef<string>('');
  const loadedOnceRef = useRef(false);
  const saveSeqRef = useRef(0);

  // Cron presets
  const CRON_PRESETS = [
    { label: t('ghost.everyHour'), value: '0 0 * * * *' },
    { label: t('ghost.every2Hours'), value: '0 0 */2 * * *' },
    { label: t('ghost.every4Hours'), value: '0 0 */4 * * *' },
    { label: t('ghost.every8Hours'), value: '0 0 */8 * * *' },
    { label: t('ghost.every12Hours'), value: '0 0 */12 * * *' },
    { label: t('ghost.dailyMidnight'), value: '0 0 0 * * *' },
    { label: t('ghost.dailyAt2AM'), value: '0 0 2 * * *' },
    { label: t('ghost.dailyAt6AM'), value: '0 0 6 * * *' },
  ];

  useEffect(() => {
    loadConfig();
  }, []);

  useEffect(() => {
    loadModelOptions();
  }, []);

  async function loadConfig() {
    setLoading(true);
    try {
      const ghostData = await getGhostConfig();
      setConfig(ghostData);

      // Initialize provider + model input from stored full model string.
      // Stored form is either null (use defaults.model) or a full model like "anthropic/claude-...".
      if (ghostData.model) {
        const parts = ghostData.model.split('/');
        if (parts.length >= 2) {
          setSelectedProvider(parts[0]);
          setModelInput(parts.slice(1).join('/'));
        } else {
          setSelectedProvider('');
          setModelInput(ghostData.model);
        }
      } else {
        setSelectedProvider('');
        setModelInput('');
      }

      lastSavedRef.current = JSON.stringify(ghostData ?? null);
      loadedOnceRef.current = true;
      
      // Determine if we should start in custom mode
      const isPreset = CRON_PRESETS.some(p => p.value === ghostData.schedule);
      if (!isPreset) {
        setCustomMode(true);
      }
    } catch (e: any) {
      setMessage(`Error: ${e.message}`);
    } finally {
      setLoading(false);
    }
  }

  async function loadModelOptions() {
    try {
      const opts = await getGhostModelOptions();
      setModelOptions(opts);

      // If no provider chosen yet, try to pick one from the default model prefix.
      // Only do this if ghost model is not explicitly set.
      if (!selectedProvider) {
        const prefix = opts.default_model.split('/')[0] || '';
        if (opts.providers.includes(prefix)) {
          setSelectedProvider(prefix);
        }
      }
    } catch {
      // ignore
    }
  }

  function toStoredGhostModel(provider: string, rawModel: string): string | null {
    const trimmed = (rawModel || '').trim();
    if (!trimmed) return null;
    if (trimmed.includes('/')) return trimmed;
    const p = (provider || '').trim();
    if (!p) return trimmed;
    return `${p}/${trimmed}`;
  }

  useEffect(() => {
    if (!config) return;
    if (!loadedOnceRef.current) return;

    const current = JSON.stringify(config);
    if (current === lastSavedRef.current) return;

    if (saveTimerRef.current) {
      window.clearTimeout(saveTimerRef.current);
    }

    saveTimerRef.current = window.setTimeout(async () => {
      if (!config) return;
      const seq = ++saveSeqRef.current;

      setSaving(true);
      setMessage('');
      try {
        const res = await updateGhostConfig(config);
        if (seq !== saveSeqRef.current) {
          return;
        }

        if (res.config) {
          lastSavedRef.current = JSON.stringify(res.config);
          setConfig(res.config);

          // Update custom mode based on new schedule
          const isPreset = CRON_PRESETS.some(p => p.value === res.config!.schedule);
          if (isPreset) {
            setCustomMode(false);
          }
        } else {
          lastSavedRef.current = JSON.stringify(config);
        }

        setMessage(res.message || 'Saved');
      } catch (e: any) {
        setMessage(`Error: ${e.message}`);
      } finally {
        if (seq === saveSeqRef.current) {
          setSaving(false);
        }
      }
    }, 600);

    return () => {
      if (saveTimerRef.current) {
        window.clearTimeout(saveTimerRef.current);
        saveTimerRef.current = null;
      }
    };
  }, [config]);

  if (loading) {
    return (
      <div className="space-y-4 animate-pulse">
        <div className="h-10 bg-muted rounded w-1/3" />
        <div className="h-8 bg-muted rounded w-full" />
        <div className="h-8 bg-muted rounded w-2/3" />
      </div>
    );
  }

  if (!config) {
    return <p className="text-muted-foreground">{t('ghost.loadFailed')}</p>;
  }

  // Active state logic
  const activePreset = CRON_PRESETS.find(p => p.value === config.schedule);
  const isCustomActive = customMode || !activePreset;

  return (
    <div className="max-w-2xl space-y-6">
      <p className="text-sm text-muted-foreground">
        {t('ghost.description')}
      </p>

      {/* Enable toggle */}
      <div className="flex items-center justify-between p-4 bg-card rounded-lg border border-border">
        <div>
          <p className="font-medium">{t('ghost.enableTitle')}</p>
          <p className="text-sm text-muted-foreground">{t('ghost.enableDesc')}</p>
        </div>
        <button
          onClick={() => setConfig({ ...config, enabled: !config.enabled })}
          className={cn(
            'relative w-11 h-6 rounded-full transition-colors',
            config.enabled ? 'bg-purple-500' : 'bg-muted'
          )}
        >
          <span
            className={cn(
              'absolute top-0.5 left-0.5 w-5 h-5 rounded-full bg-white transition-transform shadow-sm',
              config.enabled && 'translate-x-5'
            )}
          />
        </button>
      </div>

      {/* Model */}
      <div className="space-y-2">
        <label className="text-sm font-medium">{t('ghost.model')}</label>

        <div className="grid grid-cols-1 sm:grid-cols-2 gap-2">
          <div className="space-y-1">
            <label className="text-xs text-muted-foreground">Provider</label>
            <select
              value={selectedProvider}
              onChange={(e) => {
                const provider = e.target.value;
                setSelectedProvider(provider);
                const stored = toStoredGhostModel(provider, modelInput);
                setConfig({ ...config, model: stored });
              }}
              className="w-full px-3 py-2 bg-background border border-border rounded-lg text-sm focus:outline-none focus:ring-2 focus:ring-purple-500"
            >
              <option value="">(auto)</option>
              {(modelOptions?.providers || []).map((p) => (
                <option key={p} value={p}>
                  {p}
                </option>
              ))}
            </select>
          </div>

          <div className="space-y-1">
            <label className="text-xs text-muted-foreground">Model</label>
            <div className="relative">
              <input
                type="text"
                value={modelInput}
                onChange={(e) => {
                  const v = e.target.value;
                  setModelInput(v);
                  const stored = toStoredGhostModel(selectedProvider, v);
                  setConfig({ ...config, model: stored });
                }}
                placeholder={t('ghost.modelPlaceholder')}
                className="w-full px-3 py-2 bg-background border border-border rounded-lg text-sm focus:outline-none focus:ring-2 focus:ring-purple-500"
              />
              <Brain className="absolute right-3 top-2.5 w-4 h-4 text-muted-foreground pointer-events-none" />
            </div>
          </div>
        </div>

        <p className="text-xs text-muted-foreground">
          {modelInput.trim()
            ? `Using: ${toStoredGhostModel(selectedProvider, modelInput)}`
            : `Default: ${modelOptions?.default_model || ''}`}
        </p>
      </div>

      {/* Schedule */}
      <div className="space-y-3">
        <label className="text-sm font-medium">{t('ghost.schedule')}</label>
        
        <div className="grid grid-cols-1 sm:grid-cols-2 gap-2">
          {CRON_PRESETS.map((preset) => (
            <button
              key={preset.value}
              type="button"
              onClick={() => {
                setConfig({ ...config, schedule: preset.value });
                setCustomMode(false);
              }}
              className={cn(
                'px-3 py-2 text-sm text-left rounded-lg border transition-all',
                !customMode && config.schedule === preset.value
                  ? 'border-purple-500 bg-purple-50 dark:bg-purple-900/20 text-purple-700 dark:text-purple-300 ring-1 ring-purple-500'
                  : 'border-border hover:border-purple-300 hover:bg-muted/50'
              )}
            >
              <div className="font-medium">{preset.label}</div>
              <div className="text-xs text-muted-foreground font-mono mt-0.5">{preset.value}</div>
            </button>
          ))}
          <button
            type="button"
            onClick={() => {
              setCustomMode(true);
              // If switching to custom from a preset, keep the current value as starting point
            }}
            className={cn(
              'px-3 py-2 text-sm text-left rounded-lg border transition-all',
              isCustomActive
                ? 'border-purple-500 bg-purple-50 dark:bg-purple-900/20 text-purple-700 dark:text-purple-300 ring-1 ring-purple-500'
                : 'border-border hover:border-purple-300 hover:bg-muted/50'
            )}
          >
            <div className="font-medium">{t('ghost.custom')}</div>
            {isCustomActive ? (
              <input 
                type="text" 
                value={config.schedule}
                onChange={(e) => setConfig({ ...config, schedule: e.target.value })}
                className="w-full mt-1 px-2 py-1 text-xs font-mono bg-background border border-border rounded focus:outline-none focus:border-purple-500"
                onClick={(e) => e.stopPropagation()}
                placeholder="0 0 * * * *"
              />
            ) : (
              <div className="text-xs text-muted-foreground mt-0.5">{t('ghost.customHint')}</div>
            )}
          </button>
        </div>

        <div className="flex items-center gap-2 text-xs text-muted-foreground bg-muted/50 p-2 rounded">
           <Clock className="w-3.5 h-3.5" />
           <span>{t('ghost.currentSchedule')}: <code className="bg-background px-1 rounded border border-border">{config.schedule}</code></span>
        </div>
      </div>

      <div className="grid grid-cols-1 md:grid-cols-2 gap-6">
        {/* Max syncs per day */}
        <div className="space-y-2">
          <label className="text-sm font-medium">{t('ghost.maxSyncsPerDay')}</label>
          <input
            type="number"
            min={1}
            max={100}
            value={config.maxSyncsPerDay}
            onChange={(e) => setConfig({ ...config, maxSyncsPerDay: parseInt(e.target.value) || 1 })}
            className="w-full px-3 py-2 bg-background border border-border rounded-lg text-sm focus:outline-none focus:ring-2 focus:ring-purple-500"
          />
          <p className="text-xs text-muted-foreground">
            {t('ghost.maxSyncsHint')}
          </p>
        </div>

        {/* Auto social */}
        <div className="space-y-2">
          <label className="text-sm font-medium">{t('ghost.autoSocial')}</label>
          <div className="flex items-center justify-between p-3 bg-card rounded-lg border border-border">
            <span className="text-sm">{t('ghost.enableHubSync')}</span>
            <button
              onClick={() => setConfig({ ...config, autoSocial: !config.autoSocial })}
              className={cn(
                'relative w-9 h-5 rounded-full transition-colors',
                config.autoSocial ? 'bg-purple-500' : 'bg-muted'
              )}
            >
              <span
                className={cn(
                  'absolute top-0.5 left-0.5 w-4 h-4 rounded-full bg-white transition-transform shadow-sm',
                  config.autoSocial && 'translate-x-4'
                )}
              />
            </button>
          </div>
          <p className="text-xs text-muted-foreground">
            {t('ghost.autoSocialHint')}
          </p>
        </div>
      </div>

      <div className="flex items-center gap-4 pt-4 border-t border-border">
        {saving && (
          <p className="text-sm text-muted-foreground">{t('ghost.saving')}</p>
        )}
        {message && (
          <p className={cn('text-sm animate-in fade-in', message.startsWith('Error') ? 'text-red-500' : 'text-green-500')}>
            {message}
          </p>
        )}
      </div>
    </div>
  );
}

function GhostActivityPanel() {
  const [activities, setActivities] = useState<GhostActivity[]>([]);
  const [loading, setLoading] = useState(true);
  const t = useT();

  useEffect(() => {
    loadActivity();
  }, []);

  async function loadActivity() {
    setLoading(true);
    try {
      const data = await getGhostActivity(30);
      setActivities(data.activities);
    } catch {
      // ignore
    } finally {
      setLoading(false);
    }
  }

  if (loading) {
    return (
      <div className="space-y-4 animate-pulse">
        {[1, 2, 3].map((i) => (
          <div key={i} className="h-24 bg-muted rounded-lg" />
        ))}
      </div>
    );
  }

  if (activities.length === 0) {
    return (
      <div className="text-center py-16">
        <Ghost className="w-16 h-16 text-muted-foreground/30 mx-auto mb-4" />
        <h3 className="text-lg font-medium mb-1">{t('ghost.noActivityTitle')}</h3>
        <p className="text-sm text-muted-foreground max-w-md mx-auto">
          {t('ghost.noActivityDesc')}
        </p>
      </div>
    );
  }

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <p className="text-sm text-muted-foreground">{t('ghost.activityRecords', { n: activities.length })}</p>
        <button
          onClick={loadActivity}
          className="p-2 text-muted-foreground hover:text-foreground hover:bg-muted rounded-lg transition-colors"
        >
          <RefreshCw className="w-4 h-4" />
        </button>
      </div>

      <div className="space-y-3">
        {activities.map((activity) => (
          <ActivityCard key={activity.session_id} activity={activity} />
        ))}
      </div>
    </div>
  );
}

function ActivityCard({ activity }: { activity: GhostActivity }) {
  const [expanded, setExpanded] = useState(false);
  const t = useT();

  const displayTime = activity.timestamp;

  return (
    <div
      className="bg-card rounded-lg border border-border p-4 hover:border-purple-300 transition-colors cursor-pointer"
      onClick={() => setExpanded(!expanded)}
    >
      <div className="flex items-start justify-between mb-2">
        <div className="flex items-center gap-2">
          <Ghost className="w-4 h-4 text-purple-500" />
          <span className="text-sm font-medium">{displayTime}</span>
        </div>
        <div className="flex items-center gap-2 text-xs text-muted-foreground">
          <MessageSquare className="w-3.5 h-3.5" />
          <span>{t('ghost.messages', { n: activity.message_count })}</span>
        </div>
      </div>

      {/* Tool calls badges */}
      {activity.tool_calls.length > 0 && (
        <div className="flex flex-wrap gap-1 mb-2">
          {activity.tool_calls.map((tool, i) => (
            <span
              key={`${tool}-${i}`}
              className="inline-flex items-center gap-1 text-xs bg-purple-50 dark:bg-purple-900/20 text-purple-600 dark:text-purple-400 px-2 py-0.5 rounded-full"
            >
              <Wrench className="w-3 h-3" />
              {tool}
            </span>
          ))}
        </div>
      )}

      {/* Summary */}
      {activity.summary && (
        <p className={cn(
          'text-sm text-muted-foreground',
          !expanded && 'line-clamp-2'
        )}>
          {activity.summary}
        </p>
      )}

      {/* Expanded details */}
      {expanded && activity.routine_prompt && (
        <div className="mt-3 pt-3 border-t border-border">
          <p className="text-xs font-medium text-muted-foreground mb-1 flex items-center gap-1">
            <Brain className="w-3 h-3" /> {t('ghost.routinePrompt')}
          </p>
          <p className="text-xs text-muted-foreground bg-muted rounded p-2 whitespace-pre-wrap">
            {activity.routine_prompt}
          </p>
        </div>
      )}
    </div>
  );
}
