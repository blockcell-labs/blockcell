import { useEffect, useRef, useState } from 'react';
import {
  MessageCircle, Hash, Slack, Send, Phone, Building2, Wifi, Globe,
  X, ExternalLink, Save, CheckCircle, AlertCircle, Loader2, Plus, Trash2
} from 'lucide-react';
import {
  clearChannelAccountOwner,
  clearChannelOwner,
  getChannels,
  getChannelsStatus,
  getConfig,
  setChannelAccountOwner,
  setChannelOwner,
  updateConfig,
  updateChannel,
  type ChannelInfo,
  type ChannelField,
  type ChannelRuntimeStatus,
} from '@/lib/api';
import { cn } from '@/lib/utils';
import { useI18nStore, useT } from '@/lib/i18n';

const CHANNEL_ICONS: Record<string, React.ReactNode> = {
  telegram: <Send className="w-7 h-7" />,
  discord: <Hash className="w-7 h-7" />,
  slack: <Slack className="w-7 h-7" />,
  feishu: <MessageCircle className="w-7 h-7" />,
  dingtalk: <Phone className="w-7 h-7" />,
  wecom: <Building2 className="w-7 h-7" />,
  whatsapp: <Wifi className="w-7 h-7" />,
  lark: <Globe className="w-7 h-7" />,
  napcat: <MessageCircle className="w-7 h-7" />,
};

const CHANNEL_COLORS: Record<string, string> = {
  telegram: 'text-blue-400',
  discord: 'text-indigo-400',
  slack: 'text-purple-400',
  feishu: 'text-cyan-400',
  dingtalk: 'text-orange-400',
  wecom: 'text-[hsl(var(--brand-green))]',
  whatsapp: 'text-[hsl(var(--brand-green))]',
  lark: 'text-sky-400',
  napcat: 'text-green-400',
};

const GITHUB_BASE_ZH = 'https://github.com/blockcell-labs/blockcell/blob/main/docs/channels/zh/';
const GITHUB_BASE_EN = 'https://github.com/blockcell-labs/blockcell/blob/main/docs/channels/en/';

const DOC_FILES: Record<string, string> = {
  telegram: '01_telegram.md',
  discord:  '02_discord.md',
  slack:    '03_slack.md',
  feishu:   '04_feishu.md',
  dingtalk: '05_dingtalk.md',
  wecom:    '06_wecom.md',
  whatsapp: '07_whatsapp.md',
  lark:     '08_lark.md',
  napcat:   '10_napcatqq.md',
};

function extractAgentIds(config: any): string[] {
  const ids = new Set<string>();
  const agents = config?.agents?.list;
  if (Array.isArray(agents)) {
    for (const a of agents) {
      const id = typeof a?.id === 'string' ? a.id.trim() : '';
      const enabled = a?.enabled !== false;
      if (id && enabled) ids.add(id);
    }
  }
  if (ids.size === 0) ids.add('default');
  return Array.from(ids);
}

function readChannelAccounts(config: any, channelId: string): Record<string, any> {
  const accounts = config?.channels?.[channelId]?.accounts;
  if (!accounts || typeof accounts !== 'object' || Array.isArray(accounts)) return {};
  return accounts as Record<string, any>;
}

function readChannelDefaultAccountId(config: any, channelId: string): string {
  const v = config?.channels?.[channelId]?.defaultAccountId;
  return typeof v === 'string' ? v : '';
}

function primaryCredentialKey(channelId: string): string | null {
  switch (channelId) {
    case 'telegram': return 'token';
    case 'whatsapp': return 'bridgeUrl';
    case 'feishu':
    case 'lark': return 'appId';
    case 'slack':
    case 'discord': return 'botToken';
    case 'dingtalk': return 'appKey';
    case 'wecom': return 'corpId';
    case 'napcat': return 'httpUrl';
    default: return null;
  }
}

function hasConfiguredCredential(channelId: string, values: Record<string, string>): boolean {
  const key = primaryCredentialKey(channelId);
  return key ? (values[key] ?? '').trim().length > 0 : false;
}

function deriveListenerLabels(
  channelId: string,
  enabled: boolean,
  fields: Record<string, string>,
  accountDrafts: AccountDraft[],
): string[] {
  if (!enabled) return [];

  const labels = accountDrafts
    .filter((draft) => draft.enabled && draft.id.trim() && hasConfiguredCredential(channelId, draft.values))
    .map((draft) => `${channelId}:${draft.id.trim()}`);

  if (labels.length === 0 && hasConfiguredCredential(channelId, fields)) {
    labels.push(channelId);
  }

  labels.sort();
  return labels;
}

function indexChannelStatuses(statuses: ChannelRuntimeStatus[]): Record<string, ChannelRuntimeStatus> {
  return statuses.reduce<Record<string, ChannelRuntimeStatus>>((acc, status) => {
    acc[status.name] = status;
    return acc;
  }, {});
}

type AccountFieldKind = 'text' | 'number' | 'list' | 'optional';

interface AccountFieldSpec {
  key: string;
  label: string;
  secret?: boolean;
  kind?: AccountFieldKind;
}

interface AccountDraft {
  id: string;
  enabled: boolean;
  ownerAgent: string;
  values: Record<string, string>;
  extra: Record<string, any>;
}

function getAccountFieldSpecs(channelId: string): AccountFieldSpec[] {
  switch (channelId) {
    case 'telegram':
      return [
        { key: 'token', label: 'token', secret: true },
        { key: 'proxy', label: 'proxy', kind: 'optional' },
        { key: 'allowFrom', label: 'allowFrom', kind: 'list' },
      ];
    case 'whatsapp':
      return [
        { key: 'bridgeUrl', label: 'bridgeUrl' },
        { key: 'allowFrom', label: 'allowFrom', kind: 'list' },
      ];
    case 'feishu':
    case 'lark':
      return [
        { key: 'appId', label: 'appId' },
        { key: 'appSecret', label: 'appSecret', secret: true },
        { key: 'encryptKey', label: 'encryptKey', secret: true },
        { key: 'verificationToken', label: 'verificationToken', secret: true },
        { key: 'allowFrom', label: 'allowFrom', kind: 'list' },
      ];
    case 'slack':
      return [
        { key: 'botToken', label: 'botToken', secret: true },
        { key: 'appToken', label: 'appToken', secret: true },
        { key: 'channels', label: 'channels', kind: 'list' },
        { key: 'allowFrom', label: 'allowFrom', kind: 'list' },
        { key: 'pollIntervalSecs', label: 'pollIntervalSecs', kind: 'number' },
      ];
    case 'discord':
      return [
        { key: 'botToken', label: 'botToken', secret: true },
        { key: 'channels', label: 'channels', kind: 'list' },
        { key: 'allowFrom', label: 'allowFrom', kind: 'list' },
      ];
    case 'dingtalk':
      return [
        { key: 'appKey', label: 'appKey' },
        { key: 'appSecret', label: 'appSecret', secret: true },
        { key: 'robotCode', label: 'robotCode' },
        { key: 'allowFrom', label: 'allowFrom', kind: 'list' },
      ];
    case 'wecom':
      return [
        { key: 'corpId', label: 'corpId' },
        { key: 'corpSecret', label: 'corpSecret', secret: true },
        { key: 'agentId', label: 'agentId', kind: 'number' },
        { key: 'callbackToken', label: 'callbackToken', secret: true },
        { key: 'encodingAesKey', label: 'encodingAesKey', secret: true },
        { key: 'allowFrom', label: 'allowFrom', kind: 'list' },
        { key: 'pollIntervalSecs', label: 'pollIntervalSecs', kind: 'number' },
      ];
    case 'napcat':
      return [
        { key: 'mode', label: 'mode' },
        { key: 'wsUrl', label: 'wsUrl' },
        { key: 'httpUrl', label: 'httpUrl' },
        { key: 'accessToken', label: 'accessToken', secret: true },
        { key: 'serverHost', label: 'serverHost' },
        { key: 'serverPort', label: 'serverPort', kind: 'number' },
      ];
    default:
      return [];
  }
}

function valueFromRaw(raw: any, spec: AccountFieldSpec): string {
  const v = raw?.[spec.key];
  if (v === undefined || v === null) return '';
  if (spec.kind === 'list') {
    return Array.isArray(v) ? v.join(', ') : String(v);
  }
  return String(v);
}

function accountsToDrafts(
  channelId: string,
  accounts: Record<string, any>,
  accountOwners: Record<string, string> = {},
): AccountDraft[] {
  const specs = getAccountFieldSpecs(channelId);
  return Object.entries(accounts ?? {}).map(([id, raw]) => {
    const values: Record<string, string> = {};
    for (const spec of specs) {
      values[spec.key] = valueFromRaw(raw, spec);
    }
    const extra: Record<string, any> = {};
    if (raw && typeof raw === 'object' && !Array.isArray(raw)) {
      for (const [k, v] of Object.entries(raw)) {
        if (k === 'enabled') continue;
        if (specs.some((s) => s.key === k)) continue;
        extra[k] = v;
      }
    }
    return {
      id,
      enabled: raw?.enabled !== false,
      ownerAgent: typeof accountOwners?.[id] === 'string' ? accountOwners[id] : '',
      values,
      extra,
    };
  });
}

function makeEmptyAccountDraft(channelId: string): AccountDraft {
  const values: Record<string, string> = {};
  for (const spec of getAccountFieldSpecs(channelId)) {
    values[spec.key] = '';
  }
  return {
    id: '',
    enabled: true,
    ownerAgent: '',
    values,
    extra: {},
  };
}

function ToggleSwitch({
  enabled,
  onChange,
  disabled = false,
  disabledTitle,
  enabledTitle,
  disabledStateTitle,
}: {
  enabled: boolean;
  onChange: (v: boolean) => void;
  disabled?: boolean;
  disabledTitle?: string;
  enabledTitle?: string;
  disabledStateTitle?: string;
}) {
  return (
    <button
      type="button"
      disabled={disabled}
      onClick={(e) => { e.stopPropagation(); if (!disabled) onChange(!enabled); }}
      title={disabled ? (disabledTitle ?? '') : (enabled ? (enabledTitle ?? '') : (disabledStateTitle ?? ''))}
      className={cn(
        'relative inline-flex h-5 w-9 shrink-0 items-center rounded-full transition-colors focus:outline-none',
        disabled ? 'opacity-40 cursor-not-allowed' : 'cursor-pointer',
        enabled ? 'bg-[hsl(var(--brand-green))]' : 'bg-muted'
      )}
    >
      <span className={cn(
        'inline-block h-3.5 w-3.5 transform rounded-full bg-white shadow transition-transform',
        enabled ? 'translate-x-[18px]' : 'translate-x-[3px]'
      )} />
    </button>
  );
}

function ChannelCard({
  channel,
  runtimeStatus,
  onConfigure,
  onToggle,
}: {
  channel: ChannelInfo;
  runtimeStatus?: ChannelRuntimeStatus;
  onConfigure: (ch: ChannelInfo) => void;
  onToggle: (ch: ChannelInfo, enabled: boolean) => void;
}) {
  const { locale } = useI18nStore();
  const t = useT();
  const iconColor = CHANNEL_COLORS[channel.id] || 'text-[hsl(var(--brand-green))]';
  const isActive = channel.configured;
  const owner = (channel.ownerAgent ?? '').trim();
  const accountCount = channel.accounts?.length ?? 0;
  const accountOwnerOverrideCount = Object.keys(channel.accountOwners ?? {}).length;
  const listenerCount = channel.listenerCount ?? channel.listeners?.length ?? 0;
  const runtimeActive = runtimeStatus?.active ?? false;
  const runtimeDetail = runtimeStatus?.detail?.trim() ?? '';
  const docFile = DOC_FILES[channel.id] ?? '';
  const docUrl = (locale === 'zh' ? GITHUB_BASE_ZH : GITHUB_BASE_EN) + docFile;

  return (
    <div
      className={cn(
        'relative rounded-xl border p-5 flex flex-col gap-3 transition-all duration-200 cursor-pointer group',
        isActive
          ? 'border-[hsl(var(--brand-green)/0.24)] bg-[hsl(var(--brand-green)/0.04)] shadow-md shadow-[hsl(var(--brand-green)/0.08)]'
          : 'border-border/40 bg-card/60 opacity-70 hover:opacity-90'
      )}
      onClick={() => onConfigure(channel)}
    >
      <div className="flex items-start justify-between">
        <div className={cn('p-2 rounded-lg bg-muted/60', iconColor)}>
          {CHANNEL_ICONS[channel.id] ?? <Globe className="w-7 h-7" />}
        </div>
        {/* Toggle in top-right */}
        <div className="flex items-center gap-2" onClick={(e) => e.stopPropagation()}>
          <span className={cn(
            'text-xs',
            !channel.configured ? 'text-muted-foreground/50' : channel.enabled ? 'text-[hsl(var(--brand-green))]' : 'text-muted-foreground'
          )}>
            {channel.enabled ? t('channels.enabled') : t('channels.disabled')}
          </span>
          <ToggleSwitch
            enabled={channel.enabled}
            disabled={!channel.configured}
            disabledTitle={t('channels.configureFirst')}
            enabledTitle={t('channels.clickDisable')}
            disabledStateTitle={t('channels.clickEnable')}
            onChange={(v) => onToggle(channel, v)}
          />
        </div>
      </div>

      <div>
        <h3 className={cn('font-semibold text-sm', isActive ? 'text-foreground' : 'text-muted-foreground')}>
          {channel.name}
        </h3>
        <p className="text-xs text-muted-foreground mt-0.5">
          {isActive ? t('channels.clickToEdit') : t('channels.notConfigured')}
        </p>
        <p className={cn('text-xs mt-1', owner ? 'text-muted-foreground' : 'text-amber-400')}>
          {owner ? `${t('channels.owner')}: ${owner}` : t('channels.ownerMissing')}
        </p>
        {accountCount > 0 && (
          <p className="text-xs text-muted-foreground mt-0.5">
            {t('channels.accountsCount', { count: accountCount })}
          </p>
        )}
        {accountOwnerOverrideCount > 0 && (
          <p className="text-xs text-muted-foreground mt-0.5">
            {t('channels.accountOwnerOverridesCount', { count: accountOwnerOverrideCount })}
          </p>
        )}
        {listenerCount > 0 && (
          <p className="text-xs text-muted-foreground mt-0.5">
            {t('channels.listenersCount', { count: listenerCount })}
          </p>
        )}
        {runtimeStatus && (
          <p className={cn(
            'text-xs mt-0.5 flex items-start gap-1.5',
            runtimeActive ? 'text-[hsl(var(--brand-green))]' : 'text-muted-foreground'
          )}>
            <span className={cn(
              'mt-1 h-1.5 w-1.5 shrink-0 rounded-full',
              runtimeActive ? 'bg-[hsl(var(--brand-green))]' : 'bg-muted-foreground/60'
            )} />
            <span>
              {t('channels.runtimeStatus')}: {runtimeActive ? t('channels.runtimeOnline') : t('channels.runtimeOffline')}
              {runtimeDetail ? ` · ${runtimeDetail}` : ''}
            </span>
          </p>
        )}
      </div>

      <div className="flex items-center gap-2 pt-1 border-t border-border/30">
        {isActive && (
          <span className="flex items-center gap-1 text-xs text-[hsl(var(--brand-green))] font-medium">
            <span className="w-1.5 h-1.5 rounded-full bg-[hsl(var(--brand-green))] animate-pulse" />
            {t('channels.configured')}
          </span>
        )}
        <a
          href={docUrl}
          target="_blank"
          rel="noopener noreferrer"
          className="ml-auto text-xs text-muted-foreground hover:text-[hsl(var(--brand-green))] transition-colors flex items-center gap-1"
          onClick={(e) => e.stopPropagation()}
        >
          {t('channels.docs')} <ExternalLink className="w-3 h-3" />
        </a>
      </div>
    </div>
  );
}

function ChannelDrawer({
  channel,
  runtimeStatus,
  agentIds,
  initialAccounts,
  initialAccountOwners,
  initialDefaultAccountId,
  onClose,
  onSaved,
}: {
  channel: ChannelInfo;
  runtimeStatus?: ChannelRuntimeStatus;
  agentIds: string[];
  initialAccounts: Record<string, any>;
  initialAccountOwners: Record<string, string>;
  initialDefaultAccountId: string;
  onClose: () => void;
  onSaved: (
    updated: ChannelInfo,
    accounts: Record<string, any>,
    defaultAccountId: string,
    accountOwners: Record<string, string>,
  ) => void;
}) {
  const { locale } = useI18nStore();
  const t = useT();
  const [fields, setFields] = useState<Record<string, string>>(() => {
    const init: Record<string, string> = {};
    channel.fields.forEach((f: ChannelField) => { init[f.key] = f.value; });
    return init;
  });
  const [enabled, setEnabled] = useState(channel.enabled);
  const [ownerAgent, setOwnerAgent] = useState((channel.ownerAgent ?? '').trim());
  const [defaultAccountId, setDefaultAccountId] = useState((initialDefaultAccountId ?? '').trim());
  const [accountDrafts, setAccountDrafts] = useState<AccountDraft[]>(
    () => accountsToDrafts(channel.id, initialAccounts, initialAccountOwners)
  );
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [error, setError] = useState('');
  const ownerOptions = agentIds.includes(ownerAgent) || !ownerAgent
    ? agentIds
    : [...agentIds, ownerAgent];
  const accountSpecs = getAccountFieldSpecs(channel.id);
  const docFile = DOC_FILES[channel.id] ?? '';
  const docUrl = (locale === 'zh' ? GITHUB_BASE_ZH : GITHUB_BASE_EN) + docFile;

  function addAccount() {
    setAccountDrafts((prev) => [...prev, makeEmptyAccountDraft(channel.id)]);
  }

  function removeAccount(index: number) {
    setAccountDrafts((prev) => prev.filter((_, i) => i !== index));
  }

  function updateAccountDraft(index: number, patch: Partial<AccountDraft>) {
    setAccountDrafts((prev) =>
      prev.map((item, i) => (i === index ? { ...item, ...patch } : item))
    );
  }

  function updateAccountField(index: number, key: string, value: string) {
    setAccountDrafts((prev) =>
      prev.map((item, i) =>
        i === index
          ? { ...item, values: { ...item.values, [key]: value } }
          : item
      )
    );
  }

  async function handleSave() {
    setSaving(true);
    setError('');
    try {
      const chResp = await updateChannel(channel.id, fields, enabled);
      if (chResp.status !== 'ok') {
        const msg = (chResp as any)?.message;
        throw new Error(msg || t('channels.saving'));
      }

      const accountsObj: Record<string, any> = {};
      const seenIds = new Set<string>();
      for (const draft of accountDrafts) {
        const id = draft.id.trim();
        if (!id) {
          throw new Error(t('channels.emptyAccountId'));
        }
        if (seenIds.has(id)) {
          throw new Error(t('channels.duplicateAccountId', { id }));
        }
        seenIds.add(id);

        const row: Record<string, any> = { ...draft.extra, enabled: draft.enabled };
        for (const spec of accountSpecs) {
          const raw = (draft.values[spec.key] ?? '').trim();
          if (spec.kind === 'list') {
            row[spec.key] = raw
              ? raw.split(',').map((s) => s.trim()).filter((s) => s.length > 0)
              : [];
          } else if (spec.kind === 'number') {
            if (!raw) {
              row[spec.key] = 0;
            } else {
              const n = Number(raw);
              if (!Number.isFinite(n)) {
                throw new Error(t('channels.invalidNumberField', { field: spec.label }));
              }
              row[spec.key] = n;
            }
          } else if (spec.kind === 'optional') {
            row[spec.key] = raw ? raw : null;
          } else {
            row[spec.key] = raw;
          }
        }
        accountsObj[id] = row;
      }

      const defaultId = defaultAccountId.trim();
      if (defaultId && !accountsObj[defaultId]) {
        throw new Error(t('channels.defaultAccountNotFound', { id: defaultId }));
      }

      const nextAccountOwners: Record<string, string> = {};
      for (const draft of accountDrafts) {
        const id = draft.id.trim();
        const owner = draft.ownerAgent.trim();
        if (id && owner) {
          nextAccountOwners[id] = owner;
        }
      }

      const latestConfig = await getConfig();
      const nextConfig = { ...latestConfig };
      if (!nextConfig.channels || typeof nextConfig.channels !== 'object') {
        throw new Error(t('channels.configSaveFailed'));
      }
      const ch = { ...(nextConfig.channels[channel.id] ?? {}) };
      ch.accounts = accountsObj;
      ch.defaultAccountId = defaultId;
      nextConfig.channels[channel.id] = ch;

      const cfgResp = await updateConfig(nextConfig);
      if (cfgResp.status !== 'ok') {
        throw new Error((cfgResp as any)?.message || t('channels.configSaveFailed'));
      }

      const owner = ownerAgent.trim();
      if (owner) {
        const ownerResp = await setChannelOwner(channel.id, owner);
        if (ownerResp.status !== 'ok') {
          const msg = (ownerResp as any)?.message;
          throw new Error(msg || t('channels.ownerSaveFailed'));
        }
      } else {
        const ownerResp = await clearChannelOwner(channel.id);
        if (ownerResp.status !== 'ok') {
          const msg = (ownerResp as any)?.message;
          throw new Error(msg || t('channels.ownerSaveFailed'));
        }
      }

      const previousAccountOwners = channel.accountOwners ?? {};
      const accountIdsToSync = new Set<string>([
        ...Object.keys(previousAccountOwners),
        ...Object.keys(nextAccountOwners),
      ]);
      for (const accountId of accountIdsToSync) {
        const previousOwner = (previousAccountOwners[accountId] ?? '').trim();
        const nextOwner = (nextAccountOwners[accountId] ?? '').trim();
        if (previousOwner === nextOwner) continue;

        if (nextOwner) {
          const ownerResp = await setChannelAccountOwner(channel.id, accountId, nextOwner);
          if (ownerResp.status !== 'ok') {
            const msg = (ownerResp as any)?.message;
            throw new Error(msg || t('channels.accountOwnerSaveFailed'));
          }
        } else {
          const ownerResp = await clearChannelAccountOwner(channel.id, accountId);
          if (ownerResp.status !== 'ok') {
            const msg = (ownerResp as any)?.message;
            throw new Error(msg || t('channels.accountOwnerSaveFailed'));
          }
        }
      }
      setSaved(true);
      // Build updated channel object for immediate page refresh
      const updatedFields = channel.fields.map((f) => ({ ...f, value: fields[f.key] ?? f.value }));
      const derivedListeners = deriveListenerLabels(channel.id, enabled, fields, accountDrafts);
      const updated: ChannelInfo = {
        ...channel,
        enabled,
        configured: derivedListeners.length > 0,
        ownerAgent: ownerAgent.trim(),
        accountOwners: nextAccountOwners,
        defaultAccountId: defaultId,
        accounts: Object.keys(accountsObj),
        listeners: derivedListeners,
        listenerCount: derivedListeners.length,
        fields: updatedFields,
      };
      setTimeout(() => {
        setSaved(false);
        onSaved(updated, accountsObj, defaultId, nextAccountOwners);
        onClose();
      }, 900);
    } catch (e: any) {
      setError(e.message || t('channels.saving'));
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className="fixed inset-0 z-50 flex">
      <div className="flex-1 bg-black/60" onClick={onClose} />
      <div className="w-96 bg-background border-l border-border flex flex-col shadow-2xl animate-in slide-in-from-right duration-200">
        {/* Header */}
        <div className="flex items-center justify-between p-5 border-b border-border">
          <div className="flex items-center gap-3">
            <span className={cn('text-xl', CHANNEL_COLORS[channel.id] || 'text-[hsl(var(--brand-green))]')}>
              {CHANNEL_ICONS[channel.id] ?? <Globe className="w-5 h-5" />}
            </span>
            <div>
              <h2 className="font-semibold text-foreground">{channel.name}</h2>
              <p className="text-xs text-muted-foreground">{t('channels.channelConfig')}</p>
            </div>
          </div>
          <button onClick={onClose} className="p-1.5 rounded-lg hover:bg-accent transition-colors">
            <X className="w-4 h-4" />
          </button>
        </div>

        {/* Body */}
        <div className="flex-1 overflow-y-auto p-5 space-y-5">
          {/* Enable toggle */}
          <div className="flex items-center justify-between p-3 rounded-lg bg-muted/40 border border-border/40">
            <div>
              <p className="text-sm font-medium text-foreground">{t('channels.enableChannel')}</p>
              <p className="text-xs text-muted-foreground">{t('channels.enableChannelDesc')}</p>
            </div>
            <ToggleSwitch
              enabled={enabled}
              onChange={setEnabled}
            />
          </div>

          {/* Fields */}
          <div className="space-y-1.5">
            <label className="text-sm font-medium text-foreground">{t('channels.owner')}</label>
            <select
              value={ownerAgent}
              onChange={(e) => setOwnerAgent(e.target.value)}
              className="w-full bg-muted/40 border border-border rounded-lg px-3 py-2 text-sm text-foreground focus:outline-none focus:border-[hsl(var(--brand-green)/0.45)] transition-colors"
            >
              <option value="">{t('channels.ownerUnbound')}</option>
              {ownerOptions.map((id) => (
                <option key={id} value={id}>{id}</option>
              ))}
            </select>
            <p className="text-xs text-muted-foreground">{t('channels.ownerDesc')}</p>
          </div>

          <div className="p-3 rounded-lg bg-muted/30 border border-border/30 space-y-1">
            <p className="text-sm font-medium text-foreground">{t('channels.accounts')}</p>
            <p className="text-xs text-muted-foreground">
              {t('channels.defaultAccount')}: {(channel.defaultAccountId ?? '').trim() || '-'}
            </p>
            <p className="text-xs text-muted-foreground break-all">
              {t('channels.accounts')}: {(channel.accounts && channel.accounts.length > 0) ? channel.accounts.join(', ') : '-'}
            </p>
            <p className="text-xs text-muted-foreground break-all">
              {t('channels.listeners')}: {(channel.listeners && channel.listeners.length > 0) ? channel.listeners.join(', ') : t('channels.noListeners')}
            </p>
            <p className="text-xs text-muted-foreground break-all">
              {t('channels.accountOwners')}: {Object.keys(channel.accountOwners ?? {}).length > 0 ? Object.entries(channel.accountOwners ?? {}).map(([id, agent]) => `${id}→${agent}`).join(', ') : '-'}
            </p>
            <p className="text-xs text-muted-foreground break-all">
              {t('channels.runtimeStatus')}: {runtimeStatus ? (runtimeStatus.active ? t('channels.runtimeOnline') : t('channels.runtimeOffline')) : t('channels.runtimeUnknown')}
              {runtimeStatus?.detail ? ` · ${runtimeStatus.detail}` : ''}
            </p>
          </div>

          <div className="space-y-1.5">
            <label className="text-sm font-medium text-foreground">{t('channels.defaultAccount')}</label>
            <input
              type="text"
              value={defaultAccountId}
              onChange={(e) => setDefaultAccountId(e.target.value)}
              placeholder={t('channels.defaultAccountPlaceholder')}
              className="w-full bg-muted/40 border border-border rounded-lg px-3 py-2 text-sm text-foreground placeholder:text-muted-foreground focus:outline-none focus:border-[hsl(var(--brand-green)/0.45)] transition-colors"
            />
          </div>

          <div className="space-y-2">
            <div className="flex items-center justify-between">
              <label className="text-sm font-medium text-foreground">{t('channels.accountsEditorTitle')}</label>
              <button
                type="button"
                onClick={addAccount}
                className="inline-flex items-center gap-1 px-2 py-1 text-xs rounded border border-[hsl(var(--brand-green)/0.28)] text-[hsl(var(--brand-green))] hover:bg-[hsl(var(--brand-green)/0.08)]"
              >
                <Plus className="w-3.5 h-3.5" />
                {t('channels.addAccount')}
              </button>
            </div>
            <p className="text-xs text-muted-foreground">{t('channels.accountsEditorDesc')}</p>
            {accountDrafts.length === 0 && (
              <div className="text-xs text-muted-foreground border border-border/30 rounded-lg p-3">
                {t('channels.noAccounts')}
              </div>
            )}
            {accountDrafts.map((draft, index) => (
              <div key={`${index}-${draft.id}`} className="rounded-lg border border-border/40 bg-muted/20 p-3 space-y-2">
                <div className="flex items-center gap-2">
                  <input
                    type="text"
                    value={draft.id}
                    onChange={(e) => updateAccountDraft(index, { id: e.target.value })}
                    placeholder={t('channels.accountId')}
                    className="flex-1 bg-muted/40 border border-border rounded px-2 py-1.5 text-xs text-foreground placeholder:text-muted-foreground focus:outline-none focus:border-[hsl(var(--brand-green)/0.45)]"
                  />
                  <ToggleSwitch
                    enabled={draft.enabled}
                    onChange={(v) => updateAccountDraft(index, { enabled: v })}
                  />
                  <button
                    type="button"
                    onClick={() => removeAccount(index)}
                    className="p-1.5 rounded border border-red-400/30 text-red-400 hover:bg-red-400/10"
                    title={t('channels.removeAccount')}
                  >
                    <Trash2 className="w-3.5 h-3.5" />
                  </button>
                </div>
                <p className="text-[11px] text-muted-foreground">
                  {t('channels.accountEnabled')}: {draft.enabled ? t('channels.enabled') : t('channels.disabled')}
                </p>
                <div className="space-y-1">
                  <label className="text-xs text-muted-foreground">{t('channels.accountOwner')}</label>
                  <select
                    value={draft.ownerAgent}
                    onChange={(e) => updateAccountDraft(index, { ownerAgent: e.target.value })}
                    className="w-full bg-muted/40 border border-border rounded px-2 py-1.5 text-xs text-foreground focus:outline-none focus:border-[hsl(var(--brand-green)/0.45)]"
                  >
                    <option value="">{t('channels.accountOwnerFallback')}</option>
                    {ownerOptions.map((id) => (
                      <option key={`${draft.id || index}-${id}`} value={id}>{id}</option>
                    ))}
                  </select>
                </div>
                {accountSpecs.map((spec) => (
                  <div key={spec.key} className="space-y-1">
                    <label className="text-xs text-muted-foreground">{spec.label}</label>
                    <input
                      type={spec.secret ? 'password' : 'text'}
                      value={draft.values[spec.key] ?? ''}
                      onChange={(e) => updateAccountField(index, spec.key, e.target.value)}
                      className="w-full bg-muted/40 border border-border rounded px-2 py-1.5 text-xs text-foreground placeholder:text-muted-foreground focus:outline-none focus:border-[hsl(var(--brand-green)/0.45)]"
                    />
                  </div>
                ))}
              </div>
            ))}
          </div>

          {channel.fields.map((f: ChannelField) => (
            <div key={f.key} className="space-y-1.5">
              <label className="text-sm font-medium text-foreground">{f.label}</label>
              <input
                type={f.secret ? 'password' : 'text'}
                value={fields[f.key] ?? ''}
                onChange={(e) => setFields({ ...fields, [f.key]: e.target.value })}
                placeholder={f.secret ? '\u2022\u2022\u2022\u2022\u2022\u2022\u2022\u2022' : f.label}
                className="w-full bg-muted/40 border border-border rounded-lg px-3 py-2 text-sm text-foreground placeholder:text-muted-foreground focus:outline-none focus:border-[hsl(var(--brand-green)/0.45)] transition-colors"
              />
            </div>
          ))}

          {error && (
            <div className="flex items-center gap-2 text-xs text-red-400 bg-red-400/10 border border-red-400/20 rounded-lg px-3 py-2">
              <AlertCircle className="w-3.5 h-3.5 shrink-0" />
              {error}
            </div>
          )}

          {/* Doc link */}
          <a
            href={docUrl}
            target="_blank"
            rel="noopener noreferrer"
            className="flex items-center gap-2 text-xs text-muted-foreground hover:text-[hsl(var(--brand-green))] transition-colors"
          >
            <ExternalLink className="w-3.5 h-3.5" />
            {t('channels.viewDocs', { name: channel.name })}
          </a>
        </div>

        {/* Footer */}
        <div className="p-5 border-t border-border">
          <button
            onClick={handleSave}
            disabled={saving || saved}
            className={cn(
              'w-full flex items-center justify-center gap-2 py-2.5 rounded-lg text-sm font-medium transition-all',
              saved
                ? 'border border-[hsl(var(--success)/0.35)] bg-[hsl(var(--success)/0.12)] text-[hsl(var(--success))]'
                : 'bg-[hsl(var(--brand-green)/0.10)] border border-[hsl(var(--brand-green)/0.25)] text-[hsl(var(--brand-green))] hover:bg-[hsl(var(--brand-green)/0.16)]'
            )}
          >
            {saving ? (
              <><Loader2 className="w-4 h-4 animate-spin" /> {t('channels.saving')}</>
            ) : saved ? (
              <><CheckCircle className="w-4 h-4" /> {t('channels.saved')}</>
            ) : (
              <><Save className="w-4 h-4" /> {t('channels.save')}</>
            )}
          </button>
        </div>
      </div>
    </div>
  );
}

export function ChannelsPage() {
  const t = useT();
  const [channels, setChannels] = useState<ChannelInfo[]>([]);
  const [runtimeStatusById, setRuntimeStatusById] = useState<Record<string, ChannelRuntimeStatus>>({});
  const [agentIds, setAgentIds] = useState<string[]>(['default']);
  const [configSnapshot, setConfigSnapshot] = useState<any>(null);
  const [loading, setLoading] = useState(true);
  const [selected, setSelected] = useState<ChannelInfo | null>(null);
  // Track toggling state per channel to prevent double-clicks
  const togglingRef = useRef<Set<string>>(new Set());

  async function load() {
    setLoading(true);
    try {
      const [data, config, runtime] = await Promise.all([
        getChannels(),
        getConfig(),
        getChannelsStatus().catch(() => ({ channels: [] as ChannelRuntimeStatus[] })),
      ]);
      setChannels(data.channels ?? []);
      setRuntimeStatusById(indexChannelStatuses(runtime.channels ?? []));
      setAgentIds(extractAgentIds(config));
      setConfigSnapshot(config);
    } catch {
      // ignore
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => { load(); }, []);

  // Immediate optimistic toggle on card, then sync to backend
  async function handleToggle(ch: ChannelInfo, enabled: boolean) {
    if (togglingRef.current.has(ch.id)) return;
    togglingRef.current.add(ch.id);
    // Optimistic update
    setChannels((prev) => prev.map((c) => c.id === ch.id ? { ...c, enabled } : c));
    try {
      const currentFields: Record<string, string> = {};
      ch.fields.forEach((f) => { currentFields[f.key] = f.value; });
      const resp = await updateChannel(ch.id, currentFields, enabled);
      if (resp.status !== 'ok') {
        throw new Error((resp as any)?.message || 'toggle failed');
      }
    } catch {
      // Rollback on error
      setChannels((prev) => prev.map((c) => c.id === ch.id ? { ...c, enabled: !enabled } : c));
    } finally {
      togglingRef.current.delete(ch.id);
    }
  }

  // Immediate update from drawer save without refetching
  function handleSaved(
    updated: ChannelInfo,
    accounts: Record<string, any>,
    defaultAccountId: string,
    accountOwners: Record<string, string>,
  ) {
    setChannels((prev) => prev.map((c) => c.id === updated.id ? updated : c));
    setConfigSnapshot((prev: any) => {
      if (!prev || typeof prev !== 'object') return prev;
      const next = { ...prev };
      const channelsCfg = { ...(next.channels ?? {}) };
      const ch = { ...(channelsCfg[updated.id] ?? {}) };
      ch.accounts = accounts;
      ch.defaultAccountId = defaultAccountId;
      channelsCfg[updated.id] = ch;
      next.channels = channelsCfg;
      next.channelAccountOwners = {
        ...(next.channelAccountOwners ?? {}),
        [updated.id]: accountOwners,
      };
      return next;
    });
  }

  return (
    <div className="flex-1 flex flex-col overflow-hidden">
      {/* Header */}
      <div className="border-b border-border px-6 py-4">
        <h1 className="text-xl font-semibold text-foreground">{t('channels.title')}</h1>
        <p className="text-sm text-muted-foreground mt-0.5">{t('channels.subtitle')}</p>
      </div>

      <div className="flex-1 overflow-y-auto p-6">
        {loading ? (
          <div className="flex items-center justify-center py-20">
            <Loader2 className="w-6 h-6 animate-spin text-[hsl(var(--brand-green))]" />
          </div>
        ) : (
          <div className="grid grid-cols-2 gap-4 max-w-3xl">
            {channels.map((ch) => (
              <ChannelCard
                key={ch.id}
                channel={ch}
                runtimeStatus={runtimeStatusById[ch.id]}
                onConfigure={setSelected}
                onToggle={handleToggle}
              />
            ))}
          </div>
        )}
      </div>

      {selected && (
        <ChannelDrawer
          channel={selected}
          runtimeStatus={runtimeStatusById[selected.id]}
          agentIds={agentIds}
          initialAccounts={readChannelAccounts(configSnapshot, selected.id)}
          initialAccountOwners={selected.accountOwners ?? {}}
          initialDefaultAccountId={readChannelDefaultAccountId(configSnapshot, selected.id)}
          onClose={() => setSelected(null)}
          onSaved={handleSaved}
        />
      )}
    </div>
  );
}
