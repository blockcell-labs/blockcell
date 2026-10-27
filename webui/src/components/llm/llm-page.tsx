import { useEffect, useState, useCallback } from 'react';
import {
  Settings, Save, Plus, Trash2, CheckCircle, XCircle, Loader2,
  Eye, EyeOff, RefreshCw, Zap, AlertTriangle, ChevronRight,
  ToggleLeft, ToggleRight,
} from 'lucide-react';
import { getConfig, updateConfig, testProvider, reloadConfig } from '@/lib/api';
import { useT } from '@/lib/i18n';
import { mergeLlmConfig, normalizeProviders } from '@/lib/config-merge';

// ── Provider metadata ────────────────────────────────────────────────────────

interface KnownProvider {
  id: string;
  label: string;
  defaultBase: string;
  models: string[];
  keyHint: string;
}

const KNOWN_PROVIDERS: KnownProvider[] = [
  { id: 'openai',       label: 'OpenAI',              defaultBase: 'https://api.openai.com/v1',                          models: ['gpt-6-astra', 'gpt-6.1-sol', 'gpt-6-luna'],                                   keyHint: 'sk-proj-...' },
  { id: 'anthropic',    label: 'Anthropic',            defaultBase: 'https://api.anthropic.com/v1',                       models: ['claude-fable-5-1', 'claude-opus-5-5', 'claude-sonnet-5-5'],               keyHint: 'sk-ant-...' },
  { id: 'gemini',       label: 'Google Gemini',        defaultBase: 'https://generativelanguage.googleapis.com/v1beta/openai', models: ['gemini-4-argon', 'gemini-3.8-flash', 'gemini-3.6-flash', 'gemini-3.5-flash-lite'],                               keyHint: 'AIza...' },
  { id: 'kimi',         label: 'Moonshot AI (Kimi)',   defaultBase: 'https://api.moonshot.cn/v1',                         models: ['kimi-k3.1-pro', 'kimi-k3.1-pro-highspeed', 'kimi-k3'],                                   keyHint: 'sk-...' },
  { id: 'deepseek',     label: 'DeepSeek',             defaultBase: 'https://api.deepseek.com/v1',                        models: ['deepseek-flash', 'deepseek-v4-pro'],                                     keyHint: 'sk-...' },
  { id: 'xai',          label: 'xAI (Grok)',           defaultBase: 'https://api.x.ai/v1',                                models: ['grok-4.7'],              keyHint: 'xai-...' },
  { id: 'mistral',      label: 'Mistral',              defaultBase: 'https://api.mistral.ai/v1',                          models: ['mistral-large-4', 'mistral-medium-2604', 'mistral-small-2603'],              keyHint: '...' },
  { id: 'minimax',      label: 'MiniMax',              defaultBase: 'https://api.minimaxi.com/v1',                        models: ['MiniMax-M3.1-Flash', 'MiniMax-M3'],              keyHint: '...' },
  { id: 'qwen',         label: 'Qwen (千问)',          defaultBase: 'https://dashscope.aliyuncs.com/compatible-mode/v1',   models: ['qwen3.8-max', 'qwen3.8-flash'],              keyHint: 'sk-...' },
  { id: 'glm',          label: 'Z.AI (GLM)',           defaultBase: 'https://api.z.ai/api/paas/v4',                       models: ['glm-5.3', 'glm-5.3-flash', 'glm-5.2'],              keyHint: '...' },
  { id: 'groq',         label: 'Groq',                 defaultBase: 'https://api.groq.com/openai/v1',                     models: ['openai/gpt-oss-120b', 'llama-3.3-70b-versatile', 'qwen/qwen3.8-27b'],              keyHint: 'gsk_...' },
  { id: 'siliconflow',  label: 'SiliconFlow (硅基流动)', defaultBase: 'https://api.siliconflow.cn/v1',                      models: ['deepseek-ai/DeepSeek-V4.1-Flash', 'deepseek-ai/DeepSeek-V4-Pro', 'zai-org/GLM-5.3', 'Qwen/Qwen3.8-27B'],              keyHint: 'sk-...' },
  { id: 'openrouter',   label: 'OpenRouter',           defaultBase: 'https://openrouter.ai/api/v1',                       models: ['openai/gpt-6.1-sol', 'anthropic/claude-opus-5-5', 'deepseek/deepseek-v4-pro'], keyHint: 'sk-or-...' },
  { id: 'ollama',       label: 'Ollama (本地)',         defaultBase: 'http://localhost:11434/v1',                          models: ['qwen3.8', 'deepseek-v4-pro', 'glm-5.3'],              keyHint: '(无需 Key)' },
  { id: 'vllm',         label: 'vLLM (本地)',           defaultBase: 'http://localhost:8000/v1',                           models: ['custom-model'],              keyHint: 'dummy' },
];

function knownFor(id: string) { return KNOWN_PROVIDERS.find(p => p.id === id); }

// ── Model Entry Interface ────────────────────────────────────────────────────

interface ModelEntry {
  model: string;
  provider: string;
  weight: number;
  priority: number;
  toolCallMode?: 'native' | 'text' | 'none' | 'auto';
  temperature?: number;
  maxTokens?: number;
  inputPrice?: number;  // USD/1M tokens
  outputPrice?: number; // USD/1M tokens
}

// ── Main LLM Page ────────────────────────────────────────────────────────────

export function LLMPage() {
  const t = useT();
  const [providers, setProviders] = useState<Record<string, { apiKey?: string; apiBase?: string; apiType?: string; proxy?: string }>>({})
  const [modelPool, setModelPool] = useState<ModelEntry[]>([]);
  const [selectedProvider, setSelectedProvider] = useState<string>('');
  const [prevSelectedProvider, setPrevSelectedProvider] = useState<string>('');
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [saveStatus, setSaveStatus] = useState<'idle' | 'saved' | 'error'>('idle');
  const [saveMsg, setSaveMsg] = useState('');
  const [showAddProvider, setShowAddProvider] = useState(false);
  const [newProviderId, setNewProviderId] = useState('');
  const [newProviderApiType, setNewProviderApiType] = useState('openai');
  const [showKeyFor, setShowKeyFor] = useState<string>('');
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<{ ok: boolean; msg: string } | null>(null);

  const fetchAll = useCallback(async () => {
    setLoading(true);
    try {
      const cfg = await getConfig();
      const providersNormalized = normalizeProviders(cfg.providers || {});
      setProviders(providersNormalized as any);
      const defaults = cfg.agents?.defaults || {};
      const pool = defaults.modelPool || [];
      
      // If pool is empty but has legacy model, convert to pool
      if (pool.length === 0 && defaults.model) {
        const entry: ModelEntry = {
          model: defaults.model,
          provider: defaults.provider || '',
          weight: 1,
          priority: 1,
          toolCallMode: 'native',
          temperature: defaults.temperature,
          maxTokens: defaults.maxTokens,
        };
        setModelPool([entry]);
      } else {
        setModelPool(pool);
      }
      
      // Select first provider
      const providerKeys = Object.keys(providersNormalized || {});
      setSelectedProvider((current) => current && providerKeys.includes(current) ? current : (providerKeys[0] || ''));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => { fetchAll(); }, [fetchAll]);

  const handleSave = useCallback(async () => {
    setSaving(true); setSaveStatus('idle');
    try {
      const latestConfig = await getConfig();
      const providersNormalized = normalizeProviders(providers || {});
      const newConfig = mergeLlmConfig(latestConfig, providersNormalized, modelPool);
      const res = await updateConfig(newConfig);
      setSaveStatus(res.status === 'ok' ? 'saved' : 'error');
      setSaveMsg(res.status === 'ok' ? t('settings.proxySaved') : res.message || '');
      if (res.status === 'ok') {
        setProviders(providersNormalized as any);
        try {
          const reloadRes = await reloadConfig();
          if (reloadRes.status === 'ok') {
            setSaveMsg('✅ ' + t('settings.configSaved'));
          } else {
            setSaveMsg(t('settings.configSavedDesc'));
          }
        } catch (e) {
          setSaveMsg(t('settings.configSavedDesc'));
        }
      }
      setTimeout(() => setSaveStatus('idle'), 5000);
    } catch (e: any) {
      setSaveStatus('error'); setSaveMsg(e.message);
    } finally {
      setSaving(false);
    }
  }, [providers, modelPool, t]);

  const addProvider = () => {
    const id = newProviderId.trim().toLowerCase();
    if (!id || providers[id]) return;
    const kp = knownFor(id);
    setProviders(prev => ({ ...prev, [id]: { apiBase: kp?.defaultBase || '', apiKey: '', apiType: newProviderApiType } }));
    setSelectedProvider(id);
    setNewProviderId('');
    setNewProviderApiType('openai');
    setShowAddProvider(false);
  };

  const removeProvider = (id: string) => {
    if (!confirm(t('llm.deleteConfirm', { name: id }))) return;
    setProviders(prev => { const n = { ...prev }; delete n[id]; return n; });
    setModelPool(prev => prev.filter(e => e.provider !== id));
    if (selectedProvider === id) {
      const remaining = Object.keys(providers).filter(p => p !== id);
      setSelectedProvider(remaining[0] || '');
    }
  };

  const addModel = (providerId: string) => {
    const kp = knownFor(providerId);
    const newModel: ModelEntry = {
      model: kp?.models[0] || '',
      provider: providerId,
      weight: 1,
      priority: 1,
      temperature: 0.7,
      maxTokens: 8192,
    };
    setModelPool(prev => [...prev, newModel]);
  };

  const updateModel = (index: number, updates: Partial<ModelEntry>) => {
    setModelPool(prev => prev.map((m, i) => i === index ? { ...m, ...updates } : m));
  };

  const removeModel = (index: number) => {
    setModelPool(prev => prev.filter((_, i) => i !== index));
  };

  const testProviderConnection = async () => {
    if (!selectedProvider) return;
    const cfg = providers[selectedProvider];
    if (!cfg) return;

    const firstModelEntry = modelPool.find(m => m.provider === selectedProvider);
    if (!firstModelEntry?.model?.trim()) {
      setTestResult({
        ok: false,
        msg: t('llm.testConnectionNoModel'),
      });
      return;
    }

    setTesting(true); setTestResult(null);
    try {
      const kp = knownFor(selectedProvider);
      const res = await testProvider({
        model: firstModelEntry.model.trim(),
        api_key: cfg.apiKey || '',
        api_base: cfg.apiBase || kp?.defaultBase,
        proxy: cfg.proxy || undefined,
      });
      setTestResult({ ok: res.status === 'ok', msg: res.message });
    } catch (e: any) {
      setTestResult({ ok: false, msg: e.message });
    } finally {
      setTesting(false);
    }
  };

  useEffect(() => {
    if (selectedProvider !== prevSelectedProvider) {
      setTestResult(null);
      setPrevSelectedProvider(selectedProvider);
    }
  }, [selectedProvider, prevSelectedProvider]);

  const selectedProviderModels = modelPool.filter(m => m.provider === selectedProvider);
  const selectedProviderConfig = providers[selectedProvider];
  const kp = selectedProvider ? knownFor(selectedProvider) : null;
  const apiType = selectedProviderConfig?.apiType || 'openai';

  if (loading) {
    return (
      <div className="flex items-center justify-center h-full">
        <Loader2 size={22} className="animate-spin text-muted-foreground" />
      </div>
    );
  }

  return (
    <div className="flex flex-col h-full overflow-hidden">
      {/* Header */}
      <div className="border-b border-border py-4 pl-6 pr-16 flex items-center justify-between shrink-0">
        <div className="flex items-center gap-3">
          <Settings size={19} className="text-rust" />
          <div>
            <h1 className="text-base font-semibold">{t('llm.title')}</h1>
            <p className="text-[11px] text-muted-foreground">{t('llm.subtitle')}</p>
          </div>
        </div>
        <div className="flex items-center gap-2">
          <button onClick={fetchAll} className="p-2 rounded-lg hover:bg-accent text-muted-foreground" title={t('common.refresh')}>
            <RefreshCw size={14} />
          </button>
          <button
            onClick={handleSave}
            disabled={saving}
            className={`flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded-lg transition-colors disabled:opacity-50 ${
              saveStatus === 'saved' ? 'bg-[hsl(var(--brand-green)/0.10)] text-[hsl(var(--brand-green))] border border-[hsl(var(--brand-green)/0.28)]'
              : saveStatus === 'error' ? 'bg-destructive/10 text-destructive border border-destructive/30'
              : 'bg-rust text-white hover:bg-rust/90'
            }`}
          >
            {saving ? <Loader2 size={12} className="animate-spin" /> : <Save size={12} />}
            {saveStatus === 'saved' ? t('settings.configSaved') : saveStatus === 'error' ? t('common.error') : t('common.save')}
          </button>
        </div>
      </div>

      <div className="flex flex-1 overflow-hidden">
        {/* Left Sidebar - Provider List */}
        <div className="w-[250px] border-r border-border shrink-0 overflow-y-auto py-3 px-2 space-y-1">
          <div className="px-2 pb-2 flex items-center justify-between">
            <span className="text-xs font-semibold text-muted-foreground">{t('llm.provider')}</span>
            <button
              onClick={() => setShowAddProvider(true)}
              className="p-1 rounded hover:bg-accent text-muted-foreground"
              title={t('llm.addProvider')}
            >
              <Plus size={14} />
            </button>
          </div>

          {Object.keys(providers).length === 0 ? (
            <div className="px-3 py-8 text-center text-xs text-muted-foreground">
              {t('llm.noProviderSelected')}
            </div>
          ) : (
            Object.keys(providers).map(id => {
              const kp = knownFor(id);
              const modelCount = modelPool.filter(m => m.provider === id).length;
              const isActive = selectedProvider === id;
              return (
                <button
                  key={id}
                  onClick={() => setSelectedProvider(id)}
                  className={`w-full text-left px-3 py-2.5 rounded-lg transition-colors ${
                    isActive ? 'bg-rust/10 border border-rust/25' : 'hover:bg-accent border border-transparent'
                  }`}
                >
                  <div className="flex items-center justify-between">
                    <span className="text-sm font-medium truncate">{kp?.label || id}</span>
                    {isActive && <ChevronRight size={12} className="text-rust shrink-0" />}
                  </div>
                  <p className="text-[10px] text-muted-foreground mt-0.5">
                    {t('models.count', { n: modelCount })}
                  </p>
                </button>
              );
            })
          )}
        </div>

        {/* Right Panel - Provider Details & Models */}
        <div className="flex-1 overflow-y-auto">
          {!selectedProvider ? (
            <div className="flex items-center justify-center h-full text-sm text-muted-foreground">
              {t('llm.noProviderSelected')}
            </div>
          ) : (
            <div className="p-6 space-y-6 max-w-4xl">
              {/* Save message */}
              {saveStatus !== 'idle' && saveMsg && (
                <div className={`flex items-center gap-2 p-3 rounded-lg text-sm ${
                  saveStatus === 'saved' ? 'bg-[hsl(var(--brand-green)/0.10)] text-[hsl(var(--brand-green))] border border-[hsl(var(--brand-green)/0.28)]' : 'bg-destructive/10 text-destructive border border-destructive/30'
                }`}>
                  {saveStatus === 'saved' ? <CheckCircle size={13} /> : <AlertTriangle size={13} />}
                  {saveMsg}
                </div>
              )}

              {/* Provider Configuration */}
              <section className="border border-border rounded-xl overflow-hidden">
                <div className="px-5 py-3 bg-muted/20 border-b border-border flex items-center justify-between">
                  <div>
                    <h2 className="text-sm font-semibold">{kp?.label || selectedProvider} {t('llm.providerConfig')}</h2>
                    <p className="text-[10px] text-muted-foreground mt-0.5">
                      {t('llm.apiType')}: {apiType}
                    </p>
                  </div>
                  <button
                    onClick={() => removeProvider(selectedProvider)}
                    className="flex items-center gap-1 px-2 py-1 text-xs text-destructive hover:bg-destructive/10 rounded transition-colors"
                  >
                    <Trash2 size={11} />
                    {t('llm.deleteProvider')}
                  </button>
                </div>
                <div className="p-5 space-y-4">
                  {/* API Key */}
                  <div>
                    <label className="block text-xs font-medium text-muted-foreground mb-1.5">API Key</label>
                    <div className="relative">
                      <input
                        type={showKeyFor === selectedProvider ? 'text' : 'password'}
                        value={selectedProviderConfig?.apiKey || ''}
                        onChange={e => setProviders(prev => ({ ...prev, [selectedProvider]: { ...prev[selectedProvider], apiKey: e.target.value } }))}
                        placeholder={kp?.keyHint || 'API Key'}
                        // Try best-effort to prevent Chrome from autofilling random saved credentials.
                        autoComplete="new-password"
                        autoCorrect="off"
                        autoCapitalize="off"
                        spellCheck={false}
                        data-form-type="other"
                        data-lpignore="true"
                        data-1p-ignore
                        name={`blockcell-api-key-${selectedProvider}`}
                        className="w-full px-3 py-2 pr-8 text-sm bg-muted/30 border border-border rounded-lg focus:outline-none focus:ring-1 focus:ring-rust/40"
                      />
                      <button
                        type="button"
                        onClick={() => setShowKeyFor(showKeyFor === selectedProvider ? '' : selectedProvider)}
                        className="absolute right-2 top-1/2 -translate-y-1/2 text-muted-foreground hover:text-foreground"
                      >
                        {showKeyFor === selectedProvider ? <EyeOff size={13} /> : <Eye size={13} />}
                      </button>
                    </div>
                  </div>

                  {/* API Base */}
                  <div>
                    <label className="block text-xs font-medium text-muted-foreground mb-1.5">API Base URL</label>
                    <input
                      type="text"
                      value={selectedProviderConfig?.apiBase || kp?.defaultBase || ''}
                      onChange={e => setProviders(prev => ({ ...prev, [selectedProvider]: { ...prev[selectedProvider], apiBase: e.target.value } }))}
                      placeholder={kp?.defaultBase || 'https://...'}
                      className="w-full px-3 py-2 text-sm bg-muted/30 border border-border rounded-lg focus:outline-none focus:ring-1 focus:ring-rust/40"
                    />
                  </div>

                  {/* Provider Proxy */}
                  <div className="border-t border-border pt-4">
                    <div className="flex items-center justify-between mb-2">
                      <div>
                        <label className="text-xs font-medium text-muted-foreground">{t('llm.dedicatedProxy')}</label>
                        <p className="text-[10px] text-muted-foreground mt-0.5">
                          {t('llm.dedicatedProxyDesc')}
                        </p>
                      </div>
                      <button
                        type="button"
                        onClick={() => {
                          const current = providers[selectedProvider];
                          const hasProxy = current?.proxy !== undefined;
                          setProviders(prev => ({
                            ...prev,
                            [selectedProvider]: hasProxy
                              ? (() => { const { proxy: _, ...rest } = prev[selectedProvider]; return rest; })()
                              : { ...prev[selectedProvider], proxy: '' },
                          }));
                        }}
                        className="flex items-center gap-1.5 text-xs shrink-0"
                      >
                        {selectedProviderConfig?.proxy !== undefined ? (
                          <><ToggleRight size={20} className="text-rust" /><span className="text-rust font-medium">{t('llm.proxyEnabled')}</span></>
                        ) : (
                          <><ToggleLeft size={20} className="text-muted-foreground" /><span className="text-muted-foreground">{t('llm.proxyDisabled')}</span></>
                        )}
                      </button>
                    </div>
                    {selectedProviderConfig?.proxy !== undefined && (
                      <div className="space-y-1.5">
                        <input
                          type="text"
                          value={selectedProviderConfig.proxy || ''}
                          onChange={e => setProviders(prev => ({ ...prev, [selectedProvider]: { ...prev[selectedProvider], proxy: e.target.value } }))}
                          placeholder={t('llm.proxyPlaceholder')}
                          className="w-full px-3 py-2 text-sm bg-muted/30 border border-border rounded-lg focus:outline-none focus:ring-1 focus:ring-rust/40 font-mono"
                        />
                        <div className="flex items-center gap-1.5 text-[10px] text-muted-foreground">
                          <span className="font-medium">{t('llm.currentEffect')}</span>
                          {selectedProviderConfig.proxy
                            ? <span className="text-[hsl(var(--brand-green))] font-mono">{selectedProviderConfig.proxy}</span>
                            : <span className="text-amber-500">{t('llm.forceDirect')}</span>
                          }
                        </div>
                      </div>
                    )}
                    {selectedProviderConfig?.proxy === undefined && (
                      <div className="text-[10px] text-muted-foreground">
                        {t('llm.currentEffect')} {t('llm.inheritGlobal')}
                      </div>
                    )}
                  </div>

                  {/* Test Connection */}
                  <div className="flex items-center gap-2">
                    <button
                      onClick={testProviderConnection}
                      disabled={testing || (!selectedProviderConfig?.apiKey && selectedProvider !== 'ollama')}
                      className="flex items-center gap-1.5 px-3 py-1.5 text-xs rounded-lg bg-muted/50 border border-border hover:bg-accent disabled:opacity-40 transition-colors"
                    >
                      {testing ? <Loader2 size={11} className="animate-spin" /> : <Zap size={11} />}
                      {t('llm.testConnection')}
                    </button>
                    {testResult && (
                      <div className={`flex items-center gap-1.5 text-xs ${testResult.ok ? 'text-[hsl(var(--brand-green))]' : 'text-red-400'}`}>
                        {testResult.ok ? <CheckCircle size={12} /> : <XCircle size={12} />}
                        <span>{testResult.msg}</span>
                      </div>
                    )}
                  </div>
                </div>
              </section>

              {/* Models Section */}
              <section className="border border-border rounded-xl overflow-hidden">
                <div className="px-5 py-3 bg-muted/20 border-b border-border flex items-center justify-between">
                  <div>
                    <h2 className="text-sm font-semibold">{t('llm.modelPool')}</h2>
                    <p className="text-[10px] text-muted-foreground mt-0.5">
                      {t('llm.modelPoolDesc')}
                    </p>
                  </div>
                  <button
                    onClick={() => addModel(selectedProvider)}
                    className="flex items-center gap-1 px-2.5 py-1 text-xs rounded-lg bg-rust/10 border border-rust/30 text-rust hover:bg-rust/20 transition-colors"
                  >
                    <Plus size={11} />
                    {t('llm.addEntry')}
                  </button>
                </div>

                <div className="p-5">
                  {selectedProviderModels.length === 0 ? (
                    <div className="text-center py-8 text-sm text-muted-foreground border border-dashed border-border rounded-xl">
                      {t('llm.noProviderSelected')}<br/>
                      <button
                        onClick={() => addModel(selectedProvider)}
                        className="mt-2 text-rust hover:underline"
                      >
                        {t('llm.addEntry')}
                      </button>
                    </div>
                  ) : (
                    <div className="space-y-3">
                      {modelPool.map((model, index) => {
                        if (model.provider !== selectedProvider) return null;
                        return (
                          <div key={index} className="border border-border rounded-xl p-4 space-y-3 hover:border-rust/30 transition-colors">
                            {/* Model Name */}
                            <div className="flex items-start gap-2">
                              <div className="flex-1">
                                <label className="block text-xs font-medium text-muted-foreground mb-1.5">{t('llm.model')}</label>
                                <div className="flex gap-2">
                                  <input
                                    type="text"
                                    value={model.model}
                                    onChange={e => updateModel(index, { model: e.target.value })}
                                    placeholder={kp?.models[0] || 'model-name'}
                                    className="flex-1 px-3 py-1.5 text-sm bg-muted/30 border border-border rounded-lg focus:outline-none focus:ring-1 focus:ring-rust/40"
                                  />
                                  {kp && kp.models.length > 0 && (
                                    <select
                                      value=""
                                      onChange={e => e.target.value && updateModel(index, { model: e.target.value })}
                                      className="px-2 py-1.5 text-xs bg-muted/30 border border-border rounded-lg"
                                    >
                                      <option value="">{t('common.search')}</option>
                                      {kp.models.map(m => <option key={m} value={m}>{m}</option>)}
                                    </select>
                                  )}
                                </div>
                              </div>
                              <button
                                onClick={() => removeModel(index)}
                                className="p-1.5 mt-6 rounded text-muted-foreground hover:text-destructive hover:bg-destructive/10 transition-colors"
                                title={t('common.delete')}
                              >
                                <Trash2 size={13} />
                              </button>
                            </div>

                            {/* Priority & Weight */}
                            <div className="grid grid-cols-2 gap-3">
                              <div>
                                <label className="block text-xs font-medium text-muted-foreground mb-1.5">{t('llm.priority')}</label>
                                <input
                                  type="number"
                                  min={1}
                                  max={9}
                                  value={model.priority}
                                  onChange={e => updateModel(index, { priority: parseInt(e.target.value) || 1 })}
                                  className="w-full px-3 py-1.5 text-sm bg-muted/30 border border-border rounded-lg focus:outline-none focus:ring-1 focus:ring-rust/40"
                                />
                              </div>
                              <div>
                                <label className="block text-xs font-medium text-muted-foreground mb-1.5">{t('llm.weight')}</label>
                                <input
                                  type="number"
                                  min={1}
                                  max={99}
                                  value={model.weight}
                                  onChange={e => updateModel(index, { weight: parseInt(e.target.value) || 1 })}
                                  className="w-full px-3 py-1.5 text-sm bg-muted/30 border border-border rounded-lg focus:outline-none focus:ring-1 focus:ring-rust/40"
                                />
                              </div>
                            </div>

                            {/* Tool Call Mode */}
                            <div>
                              <label className="block text-xs font-medium text-muted-foreground mb-1.5">
                                {t('llm.toolCallMode')}
                              </label>
                              <div className="mb-1.5 text-[10px] text-muted-foreground">
                                {t('llm.toolCallModeDesc')}
                              </div>
                              <select
                                value={model.toolCallMode ?? 'native'}
                                onChange={e => updateModel(index, { toolCallMode: e.target.value as ModelEntry['toolCallMode'] })}
                                className="w-full px-3 py-1.5 text-sm bg-muted/30 border border-border rounded-lg focus:outline-none focus:ring-1 focus:ring-rust/40"
                              >
                                <option value="native">{t('llm.toolCallMode.native')}</option>
                                <option value="text">{t('llm.toolCallMode.text')}</option>
                                <option value="none">{t('llm.toolCallMode.none')}</option>
                                <option value="auto">{t('llm.toolCallMode.auto')}</option>
                              </select>
                              <div className="mt-1 text-[10px] text-muted-foreground">
                                {t('llm.toolCallMode.help')}
                              </div>
                            </div>

                            {/* Temperature & Max Tokens */}
                            <div className="grid grid-cols-2 gap-3">
                              <div>
                                <label className="block text-xs font-medium text-muted-foreground mb-1.5">
                                  Temperature {model.temperature !== undefined && `(${model.temperature})`}
                                </label>
                                <input
                                  type="range"
                                  min="0"
                                  max="2"
                                  step="0.05"
                                  value={model.temperature ?? 0.7}
                                  onChange={e => updateModel(index, { temperature: parseFloat(e.target.value) })}
                                  className="w-full accent-rust"
                                />
                                <div className="flex justify-between text-[9px] text-muted-foreground mt-0.5">
                                  <span>0</span><span>1</span><span>2</span>
                                </div>
                              </div>
                              <div>
                                <label className="block text-xs font-medium text-muted-foreground mb-1.5">Max Tokens</label>
                                <input
                                  type="number"
                                  value={model.maxTokens ?? 8192}
                                  onChange={e => updateModel(index, { maxTokens: parseInt(e.target.value) || 8192 })}
                                  min={256}
                                  max={200000}
                                  step={256}
                                  className="w-full px-3 py-1.5 text-sm bg-muted/30 border border-border rounded-lg focus:outline-none focus:ring-1 focus:ring-rust/40"
                                />
                              </div>
                            </div>

                            {/* Input Price */}
                            <div>
                              <label className="block text-xs font-medium text-muted-foreground mb-1.5">
                                Input Price (USD/1M tokens)
                              </label>
                              <input
                                type="number"
                                value={model.inputPrice ?? ''}
                                onChange={e => updateModel(index, { inputPrice: e.target.value ? parseFloat(e.target.value) : undefined })}
                                placeholder="例：0.15"
                                step="0.01"
                                min="0"
                                className="w-full px-2.5 py-1.5 text-xs bg-muted/30 border border-border rounded-lg focus:outline-none focus:ring-1 focus:ring-rust/40"
                              />
                            </div>

                            {/* Output Price */}
                            <div>
                              <label className="block text-xs font-medium text-muted-foreground mb-1.5">
                                Output Price (USD/1M tokens)
                              </label>
                              <input
                                type="number"
                                value={model.outputPrice ?? ''}
                                onChange={e => updateModel(index, { outputPrice: e.target.value ? parseFloat(e.target.value) : undefined })}
                                placeholder="例：0.60"
                                step="0.01"
                                min="0"
                                className="w-full px-2.5 py-1.5 text-xs bg-muted/30 border border-border rounded-lg focus:outline-none focus:ring-1 focus:ring-rust/40"
                              />
                            </div>
                          </div>
                        );
                      })}
                    </div>
                  )}
                </div>
              </section>

            </div>
          )}
        </div>
      </div>

      {/* Add Provider Modal */}
      {showAddProvider && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4">
          <div className="bg-card border border-border rounded-2xl shadow-2xl w-full max-w-lg p-5 space-y-4">
            <h3 className="text-sm font-semibold">{t('llm.addProvider')}</h3>
            
            <div className="flex flex-wrap gap-1.5">
              {KNOWN_PROVIDERS.filter(p => !providers[p.id]).map(p => (
                <button
                  key={p.id}
                  onClick={() => setNewProviderId(p.id)}
                  className={`px-2.5 py-1.5 text-xs rounded-lg border transition-colors ${
                    newProviderId === p.id ? 'bg-rust/15 border-rust/50 text-rust' : 'bg-muted/30 border-border hover:bg-accent text-muted-foreground'
                  }`}
                >
                  {p.label}
                </button>
              ))}
            </div>

            <div>
              <label className="block text-xs font-medium text-muted-foreground mb-1.5">{t('llm.provider')} ID</label>
              <input
                type="text"
                value={newProviderId}
                onChange={e => setNewProviderId(e.target.value)}
                onKeyDown={e => e.key === 'Enter' && addProvider()}
                placeholder="例：zhipu, groq, vllm"
                className="w-full px-3 py-2 text-sm bg-muted/30 border border-border rounded-lg focus:outline-none focus:ring-1 focus:ring-rust/40"
              />
            </div>

            <div>
              <label className="block text-xs font-medium text-muted-foreground mb-2">{t('llm.apiType')}</label>
              <div className="grid grid-cols-2 gap-2">
                {[
                  { value: 'openai', label: 'OpenAI Compatible', desc: 'OpenAI, DeepSeek, Kimi, vLLM...' },
                  { value: 'anthropic', label: 'Anthropic Native', desc: 'Claude series' },
                  { value: 'gemini', label: 'Gemini Native', desc: 'Google Gemini' },
                  { value: 'ollama', label: 'Ollama Local', desc: 'Local models' },
                ].map(type => (
                  <button
                    key={type.value}
                    onClick={() => setNewProviderApiType(type.value)}
                    className={`p-2.5 text-left rounded-lg border transition-all ${
                      newProviderApiType === type.value
                        ? 'bg-rust/10 border-rust/50 ring-1 ring-rust/30'
                        : 'bg-muted/20 border-border hover:bg-muted/40'
                    }`}
                  >
                    <div className="text-xs font-medium">{type.label}</div>
                    <div className="text-[10px] text-muted-foreground mt-0.5">{type.desc}</div>
                  </button>
                ))}
              </div>
            </div>

            <div className="flex gap-2 justify-end">
              <button
                onClick={() => { setShowAddProvider(false); setNewProviderId(''); }}
                className="px-3 py-1.5 text-xs rounded-lg border border-border hover:bg-accent transition-colors"
              >
                {t('common.cancel')}
              </button>
              <button
                onClick={addProvider}
                disabled={!newProviderId.trim()}
                className="px-3 py-1.5 text-xs rounded-lg bg-rust text-white hover:bg-rust/90 disabled:opacity-40 transition-colors"
              >
                {t('common.create')}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
