import { useEffect, useState } from 'react';
import {
  Save, RefreshCw, FlaskConical, Loader2, Globe, Sun, Moon, Monitor,
  ExternalLink, LogOut, ChevronRight, Languages, Palette, FileCode, Info, X,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import { getConfig, updateConfig, testProvider, getHealth, logout } from '@/lib/api';
import { useThemeStore } from '@/lib/store';
import { useI18nStore, useT, type Locale } from '@/lib/i18n';

// ── Config Editor sub-page ──
function ConfigEditor({ onBack, t }: { onBack: () => void; t: (k: string, p?: Record<string, string | number>) => string }) {
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [testing, setTesting] = useState(false);
  const [message, setMessage] = useState<{ type: 'success' | 'error'; text: string } | null>(null);
  const [editJson, setEditJson] = useState('');
  const [restartNoticeOpen, setRestartNoticeOpen] = useState(false);

  function RestartNoticeDialog({
    open,
    onClose,
  }: {
    open: boolean;
    onClose: () => void;
  }) {
    if (!open) return null;
    return (
      <div className="fixed inset-0 z-50 flex items-center justify-center">
        <div className="absolute inset-0 bg-black/50" onClick={onClose} />
        <div className="relative bg-card border border-border rounded-xl shadow-xl p-6 w-full max-w-sm mx-4">
          <h3 className="text-base font-semibold mb-1">配置已保存</h3>
          <p className="text-sm text-muted-foreground mb-5">
            配置文件已更新，但当前运行中的 gateway 不会自动热重载配置。
            <br />
            请重启 blockcell gateway 后再验证是否生效。
          </p>
          <div className="flex justify-end gap-2">
            <button
              onClick={onClose}
              className="px-4 py-1.5 text-sm rounded-lg bg-primary text-primary-foreground hover:bg-primary/90"
            >
              我知道了
            </button>
          </div>
        </div>
      </div>
    );
  }

  useEffect(() => {
    fetchConfig();
  }, []);

  async function fetchConfig() {
    setLoading(true);
    try {
      const data = await getConfig();
      setEditJson(JSON.stringify(data, null, 2));
    } catch (e: any) {
      setMessage({ type: 'error', text: e.message });
    } finally {
      setLoading(false);
    }
  }

  async function handleSave() {
    setSaving(true);
    setMessage(null);
    try {
      const parsed = JSON.parse(editJson);
      await updateConfig(parsed);
      setMessage({ type: 'success', text: t('settings.configSaved') });
      setRestartNoticeOpen(true);
    } catch (e: any) {
      setMessage({ type: 'error', text: e.message });
    } finally {
      setSaving(false);
    }
  }

  async function handleTestProvider() {
    setTesting(true);
    setMessage(null);
    try {
      const parsed = JSON.parse(editJson);
      const model = parsed?.agents?.defaults?.model || '';
      const providerName = Object.keys(parsed?.providers || {})[0] || 'openai';
      const providerConf = parsed?.providers?.[providerName] || {};
      const result = await testProvider({
        model,
        api_key: providerConf.apiKey || providerConf.api_key || '',
        api_base: providerConf.apiBase || providerConf.api_base,
      });
      setMessage({ type: 'success', text: result.message || 'Provider test passed' });
    } catch (e: any) {
      setMessage({ type: 'error', text: e.message });
    } finally {
      setTesting(false);
    }
  }

  return (
    <div className="flex flex-col h-full">
      <RestartNoticeDialog open={restartNoticeOpen} onClose={() => setRestartNoticeOpen(false)} />
      <div className="border-b border-border px-6 py-4 flex items-center justify-between">
        <div className="flex items-center gap-3">
          <button onClick={onBack} className="p-1.5 rounded-lg hover:bg-accent text-muted-foreground">
            <ChevronRight size={16} className="rotate-180" />
          </button>
          <h1 className="text-lg font-semibold">{t('settings.configuration')}</h1>
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={handleTestProvider}
            disabled={testing}
            className="flex items-center gap-1.5 px-3 py-1.5 text-sm rounded-lg border border-border hover:bg-accent transition-colors disabled:opacity-50"
          >
            {testing ? <Loader2 size={14} className="animate-spin" /> : <FlaskConical size={14} />}
            {t('settings.testProvider')}
          </button>
          <button
            onClick={handleSave}
            disabled={saving}
            className="flex items-center gap-1.5 px-3 py-1.5 text-sm rounded-lg bg-primary text-primary-foreground hover:bg-primary/90 transition-colors disabled:opacity-50"
          >
            {saving ? <Loader2 size={14} className="animate-spin" /> : <Save size={14} />}
            {t('common.save')}
          </button>
          <button onClick={fetchConfig} className="p-2 rounded-lg hover:bg-accent text-muted-foreground">
            <RefreshCw size={16} className={loading ? 'animate-spin' : ''} />
          </button>
        </div>
      </div>

      {message && (
        <div className={cn(
          'mx-6 mt-4 px-4 py-2 rounded-lg text-sm flex items-center justify-between',
          message.type === 'success' ? 'bg-cyber/10 text-cyber border border-cyber/20' : 'bg-red-500/10 text-red-500 border border-red-500/20'
        )}>
          <span>{message.text}</span>
          <button onClick={() => setMessage(null)} className="p-0.5 hover:opacity-70"><X size={14} /></button>
        </div>
      )}

      <div className="flex-1 overflow-y-auto p-6">
        <div className="w-full">
          {loading ? (
            <div className="flex items-center justify-center h-64">
              <Loader2 size={24} className="animate-spin text-muted-foreground" />
            </div>
          ) : (
            <textarea
              value={editJson}
              onChange={(e) => setEditJson(e.target.value)}
              className="w-full h-[calc(100vh-220px)] bg-card border border-border rounded-lg p-4 font-mono text-sm resize-none outline-none focus:ring-1 focus:ring-ring"
              spellCheck={false}
            />
          )}
        </div>
      </div>
    </div>
  );
}

// ── Main Settings page ──
export function ConfigPage() {
  const [showEditor, setShowEditor] = useState(false);
  const [version, setVersion] = useState('');
  const [logoutConfirm, setLogoutConfirm] = useState(false);
  const { theme, setTheme } = useThemeStore();
  const { locale, setLocale } = useI18nStore();
  const t = useT();

  useEffect(() => {
    getHealth().then((h) => setVersion(h.version)).catch(() => {});
  }, []);

  if (showEditor) {
    return <ConfigEditor onBack={() => setShowEditor(false)} t={t} />;
  }

  const languages: { value: Locale; label: string }[] = [
    { value: 'en', label: 'English' },
    { value: 'zh', label: '中文' },
  ];

  const themes: { value: 'dark' | 'light' | 'system'; label: string; icon: typeof Sun }[] = [
    { value: 'dark', label: t('settings.themeDark'), icon: Moon },
    { value: 'light', label: t('settings.themeLight'), icon: Sun },
    { value: 'system', label: t('settings.themeSystem'), icon: Monitor },
  ];

  return (
    <div className="flex flex-col h-full">
      <div className="border-b border-border px-6 py-4">
        <h1 className="text-lg font-semibold">{t('settings.title')}</h1>
      </div>

      <div className="flex-1 overflow-y-auto p-6">
        <div className="space-y-8">

          {/* ── General ── */}
          <section>
            <h2 className="text-sm font-semibold text-muted-foreground uppercase tracking-wider mb-4 flex items-center gap-2">
              <Languages size={14} />
              {t('settings.general')}
            </h2>
            <div className="bg-card border border-border rounded-xl divide-y divide-border">
              {/* Language */}
              <div className="flex items-center justify-between px-5 py-4">
                <div>
                  <div className="text-sm font-medium">{t('settings.language')}</div>
                  <div className="text-xs text-muted-foreground mt-0.5">{t('settings.languageDesc')}</div>
                </div>
                <div className="flex items-center gap-1 bg-accent/50 rounded-lg p-0.5">
                  {languages.map((lang) => (
                    <button
                      key={lang.value}
                      onClick={() => setLocale(lang.value)}
                      className={cn(
                        'px-3 py-1.5 text-xs rounded-md transition-colors',
                        locale === lang.value
                          ? 'bg-primary text-primary-foreground shadow-sm'
                          : 'text-muted-foreground hover:text-foreground'
                      )}
                    >
                      {lang.label}
                    </button>
                  ))}
                </div>
              </div>

              {/* Theme */}
              <div className="flex items-center justify-between px-5 py-4">
                <div>
                  <div className="text-sm font-medium">{t('settings.theme')}</div>
                  <div className="text-xs text-muted-foreground mt-0.5">{t('settings.themeDesc')}</div>
                </div>
                <div className="flex items-center gap-1 bg-accent/50 rounded-lg p-0.5">
                  {themes.map((th) => (
                    <button
                      key={th.value}
                      onClick={() => setTheme(th.value)}
                      className={cn(
                        'flex items-center gap-1.5 px-3 py-1.5 text-xs rounded-md transition-colors',
                        theme === th.value
                          ? 'bg-primary text-primary-foreground shadow-sm'
                          : 'text-muted-foreground hover:text-foreground'
                      )}
                    >
                      <th.icon size={12} />
                      {th.label}
                    </button>
                  ))}
                </div>
              </div>
            </div>
          </section>

          {/* ── Configuration ── */}
          <section>
            <h2 className="text-sm font-semibold text-muted-foreground uppercase tracking-wider mb-4 flex items-center gap-2">
              <FileCode size={14} />
              {t('settings.configuration')}
            </h2>
            <div className="bg-card border border-border rounded-xl">
              <button
                onClick={() => setShowEditor(true)}
                className="w-full flex items-center justify-between px-5 py-4 hover:bg-accent/30 transition-colors rounded-xl"
              >
                <div className="text-left">
                  <div className="text-sm font-medium">{t('settings.editConfig')}</div>
                  <div className="text-xs text-muted-foreground mt-0.5">{t('settings.configDesc')}</div>
                </div>
                <ChevronRight size={16} className="text-muted-foreground" />
              </button>
            </div>
          </section>

          {/* ── About ── */}
          <section>
            <h2 className="text-sm font-semibold text-muted-foreground uppercase tracking-wider mb-4 flex items-center gap-2">
              <Info size={14} />
              {t('settings.about')}
            </h2>
            <div className="bg-card border border-border rounded-xl divide-y divide-border">
              {/* Version */}
              <div className="flex items-center justify-between px-5 py-4">
                <div className="text-sm font-medium">{t('settings.version')}</div>
                <div className="text-sm text-muted-foreground font-mono">{version || '...'}</div>
              </div>

              {/* Website */}
              <div className="flex items-center justify-between px-5 py-4">
                <div className="text-sm font-medium">{t('settings.website')}</div>
                <a
                  href="https://blockcell.dev"
                  target="_blank"
                  rel="noopener noreferrer"
                  className="flex items-center gap-1.5 text-sm text-primary hover:underline"
                >
                  blockcell.dev
                  <ExternalLink size={12} />
                </a>
              </div>

              {/* Logout */}
              <div className="flex items-center justify-between px-5 py-4">
                <div>
                  <div className="text-sm font-medium">{t('settings.security')}</div>
                  <div className="text-xs text-muted-foreground mt-0.5">{t('settings.logoutDesc')}</div>
                </div>
                <button
                  onClick={() => setLogoutConfirm(true)}
                  className="flex items-center gap-1.5 px-3 py-1.5 text-sm rounded-lg border border-destructive/30 text-destructive hover:bg-destructive/10 transition-colors"
                >
                  <LogOut size={14} />
                  {t('settings.logoutBtn')}
                </button>
              </div>
            </div>
          </section>

        </div>
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
    </div>
  );
}
