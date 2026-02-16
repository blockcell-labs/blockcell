import { useEffect, useState, useCallback } from 'react';
import { Plus, Trash2, Play, RefreshCw, Clock, Loader2, X, CheckCircle2, AlertCircle } from 'lucide-react';
import { cn } from '@/lib/utils';
import { getCronJobs, createCronJob, deleteCronJob, runCronJob } from '@/lib/api';
import { useT } from '@/lib/i18n';

// ── Toast notification ──
interface Toast {
  id: number;
  type: 'success' | 'error';
  message: string;
}

function ToastContainer({ toasts, onDismiss }: { toasts: Toast[]; onDismiss: (id: number) => void }) {
  return (
    <div className="fixed top-4 right-4 z-50 flex flex-col gap-2">
      {toasts.map((t) => (
        <div
          key={t.id}
          className={cn(
            'flex items-center gap-2 px-4 py-3 rounded-lg shadow-lg text-sm animate-in slide-in-from-top-2 fade-in duration-200',
            t.type === 'success' ? 'bg-emerald-600 text-white' : 'bg-red-600 text-white'
          )}
        >
          {t.type === 'success' ? <CheckCircle2 size={16} /> : <AlertCircle size={16} />}
          <span className="flex-1">{t.message}</span>
          <button onClick={() => onDismiss(t.id)} className="p-0.5 hover:opacity-70">
            <X size={14} />
          </button>
        </div>
      ))}
    </div>
  );
}

// ── Confirm dialog ──
function ConfirmDialog({
  open,
  title,
  description,
  confirmLabel,
  cancelLabel = 'Cancel',
  variant,
  onConfirm,
  onCancel,
}: {
  open: boolean;
  title: string;
  description: string;
  confirmLabel: string;
  cancelLabel?: string;
  variant: 'danger' | 'primary';
  onConfirm: () => void;
  onCancel: () => void;
}) {
  if (!open) return null;
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      <div className="absolute inset-0 bg-black/50" onClick={onCancel} />
      <div className="relative bg-card border border-border rounded-xl shadow-xl p-6 w-full max-w-sm mx-4">
        <h3 className="text-base font-semibold mb-1">{title}</h3>
        <p className="text-sm text-muted-foreground mb-5">{description}</p>
        <div className="flex justify-end gap-2">
          <button
            onClick={onCancel}
            className="px-4 py-1.5 text-sm rounded-lg border border-border hover:bg-accent"
          >
            {cancelLabel}
          </button>
          <button
            onClick={onConfirm}
            className={cn(
              'px-4 py-1.5 text-sm rounded-lg text-white',
              variant === 'danger'
                ? 'bg-red-600 hover:bg-red-700'
                : 'bg-primary hover:bg-primary/90'
            )}
          >
            {confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}

export function CronPage() {
  const t = useT();
  const [jobs, setJobs] = useState<any[]>([]);
  const [loading, setLoading] = useState(true);
  const [showCreate, setShowCreate] = useState(false);
  const [newJob, setNewJob] = useState({
    name: '',
    message: '',
    every_seconds: '',
    cron_expr: '',
    skill_name: '',
  });
  const [toasts, setToasts] = useState<Toast[]>([]);
  const [confirm, setConfirm] = useState<{
    open: boolean;
    title: string;
    description: string;
    confirmLabel: string;
    variant: 'danger' | 'primary';
    onConfirm: () => void;
  }>({ open: false, title: '', description: '', confirmLabel: '', variant: 'primary', onConfirm: () => {} });

  const addToast = useCallback((type: 'success' | 'error', message: string) => {
    const id = Date.now();
    setToasts((prev) => [...prev, { id, type, message }]);
    setTimeout(() => setToasts((prev) => prev.filter((t) => t.id !== id)), 3000);
  }, []);

  const dismissToast = useCallback((id: number) => {
    setToasts((prev) => prev.filter((t) => t.id !== id));
  }, []);

  const closeConfirm = useCallback(() => {
    setConfirm((c) => ({ ...c, open: false }));
  }, []);

  useEffect(() => {
    fetchJobs();
  }, []);

  async function fetchJobs() {
    setLoading(true);
    try {
      const data = await getCronJobs();
      setJobs(data.jobs || []);
    } catch {
      // ignore
    } finally {
      setLoading(false);
    }
  }

  async function handleCreate() {
    try {
      const payload: any = {
        name: newJob.name,
        message: newJob.message,
      };
      if (newJob.every_seconds) payload.every_seconds = parseInt(newJob.every_seconds);
      if (newJob.cron_expr) payload.cron_expr = newJob.cron_expr;
      if (newJob.skill_name) payload.skill_name = newJob.skill_name;
      await createCronJob(payload);
      setShowCreate(false);
      setNewJob({ name: '', message: '', every_seconds: '', cron_expr: '', skill_name: '' });
      fetchJobs();
    } catch {
      // ignore
    }
  }

  function handleDelete(id: string) {
    const job = jobs.find((j) => j.id === id);
    setConfirm({
      open: true,
      title: t('cron.deleteCron'),
      description: t('cron.deleteConfirm', { name: job?.name || id }),
      confirmLabel: t('common.delete'),
      variant: 'danger',
      onConfirm: async () => {
        closeConfirm();
        try {
          await deleteCronJob(id);
          setJobs((prev) => prev.filter((j) => j.id !== id));
          addToast('success', t('cron.jobDeleted', { name: job?.name || id }));
        } catch (e: any) {
          addToast('error', e?.message || t('cron.deleteFailed'));
        }
      },
    });
  }

  function handleRun(id: string) {
    const job = jobs.find((j) => j.id === id);
    setConfirm({
      open: true,
      title: t('cron.runCron'),
      description: t('cron.runConfirm', { name: job?.name || id }),
      confirmLabel: t('common.run'),
      variant: 'primary',
      onConfirm: async () => {
        closeConfirm();
        try {
          await runCronJob(id);
          addToast('success', t('cron.jobTriggered', { name: job?.name || id }));
        } catch (e: any) {
          addToast('error', e?.message || t('cron.runFailed'));
        }
      },
    });
  }

  function formatSchedule(job: any): string {
    const s = job.schedule;
    if (!s) return 'unknown';
    if (s.expr) return `cron: ${s.expr}`;
    const everyMs = s.every_ms ?? s.everyMs;
    if (everyMs) return `every ${Math.round(everyMs / 1000)}s`;
    const atMs = s.at_ms ?? s.atMs;
    if (atMs) return `at ${new Date(atMs).toLocaleString()}`;
    return 'unknown';
  }

  return (
    <div className="flex flex-col h-full">
      <ToastContainer toasts={toasts} onDismiss={dismissToast} />
      <ConfirmDialog
        open={confirm.open}
        title={confirm.title}
        description={confirm.description}
        confirmLabel={confirm.confirmLabel}
        variant={confirm.variant}
        onConfirm={confirm.onConfirm}
        onCancel={closeConfirm}
      />
      <div className="border-b border-border px-6 py-4 flex items-center justify-between">
        <div>
          <h1 className="text-lg font-semibold">{t('cron.title')}</h1>
          <p className="text-sm text-muted-foreground">{jobs.length} scheduled jobs</p>
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={() => setShowCreate(!showCreate)}
            className="flex items-center gap-1.5 px-3 py-1.5 text-sm rounded-lg bg-primary text-primary-foreground hover:bg-primary/90"
          >
            <Plus size={14} /> {t('cron.create')}
          </button>
          <button
            onClick={fetchJobs}
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
              value={newJob.name}
              onChange={(e) => setNewJob({ ...newJob, name: e.target.value })}
              placeholder="Job name"
              className="px-3 py-1.5 text-sm bg-background border border-border rounded-lg outline-none focus:ring-1 focus:ring-ring"
            />
            <input
              value={newJob.skill_name}
              onChange={(e) => setNewJob({ ...newJob, skill_name: e.target.value })}
              placeholder="Skill name (optional)"
              className="px-3 py-1.5 text-sm bg-background border border-border rounded-lg outline-none focus:ring-1 focus:ring-ring"
            />
          </div>
          <textarea
            value={newJob.message}
            onChange={(e) => setNewJob({ ...newJob, message: e.target.value })}
            placeholder="Message / task content"
            rows={2}
            className="w-full px-3 py-1.5 text-sm bg-background border border-border rounded-lg outline-none focus:ring-1 focus:ring-ring resize-none"
          />
          <div className="flex items-center gap-3">
            <input
              value={newJob.every_seconds}
              onChange={(e) => setNewJob({ ...newJob, every_seconds: e.target.value })}
              placeholder="Every N seconds"
              type="number"
              className="px-3 py-1.5 text-sm bg-background border border-border rounded-lg outline-none focus:ring-1 focus:ring-ring w-40"
            />
            <span className="text-xs text-muted-foreground">or</span>
            <input
              value={newJob.cron_expr}
              onChange={(e) => setNewJob({ ...newJob, cron_expr: e.target.value })}
              placeholder="Cron expression (e.g. 0 9 * * *)"
              className="flex-1 px-3 py-1.5 text-sm bg-background border border-border rounded-lg outline-none focus:ring-1 focus:ring-ring"
            />
            <button
              onClick={handleCreate}
              disabled={!newJob.name || !newJob.message}
              className="px-4 py-1.5 text-sm rounded-lg bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
            >
              {t('common.create')}
            </button>
          </div>
        </div>
      )}

      {/* Job list */}
      <div className="flex-1 overflow-y-auto p-6">
        {loading ? (
          <div className="flex items-center justify-center h-32">
            <Loader2 size={24} className="animate-spin text-muted-foreground" />
          </div>
        ) : jobs.length === 0 ? (
          <div className="flex flex-col items-center justify-center h-32 text-muted-foreground">
            <Clock size={32} className="mb-2 opacity-30" />
            <p className="text-sm">{t('cron.empty')}</p>
          </div>
        ) : (
          <div className="space-y-2">
            {jobs.map((job: any) => (
              <div key={job.id} className="group border border-border rounded-lg p-4 bg-card">
                <div className="flex items-start justify-between gap-3">
                  <div className="flex-1 min-w-0">
                    <div className="flex items-center gap-2">
                      <span className="font-medium text-sm">{job.name}</span>
                      <span
                        className={cn(
                          'text-[10px] px-1.5 py-0.5 rounded-full',
                          job.enabled !== false ? 'bg-cyber/10 text-cyber' : 'bg-muted text-muted-foreground'
                        )}
                      >
                        {job.enabled !== false ? 'enabled' : 'disabled'}
                      </span>
                    </div>
                    <p className="text-xs text-muted-foreground mt-1 font-mono">{formatSchedule(job)}</p>
                    {job.payload?.message && (
                      <p className="text-xs text-muted-foreground mt-1 truncate">{job.payload.message}</p>
                    )}
                    {job.state?.last_status && (
                      <p className="text-xs mt-1">
                        Last: <span className={job.state.last_status === 'ok' ? 'text-cyber' : 'text-red-500'}>
                          {job.state.last_status}
                        </span>
                        {job.state.last_error && (
                          <span className="text-red-500 ml-2">{job.state.last_error}</span>
                        )}
                      </p>
                    )}
                  </div>
                  <div className="flex items-center gap-1">
                    <button
                      onClick={() => handleRun(job.id)}
                      className="p-1.5 rounded hover:bg-accent text-muted-foreground"
                      title="Run now"
                    >
                      <Play size={14} />
                    </button>
                    <button
                      onClick={() => handleDelete(job.id)}
                      className="p-1.5 rounded hover:bg-destructive/20 text-destructive opacity-0 group-hover:opacity-100 transition-opacity"
                      title="Delete"
                    >
                      <Trash2 size={14} />
                    </button>
                  </div>
                </div>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
