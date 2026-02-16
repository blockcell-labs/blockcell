import { useEffect, useState } from 'react';
import { RefreshCw, CheckCircle, XCircle, Clock, Loader2 } from 'lucide-react';
import { cn } from '@/lib/utils';
import { getTasks } from '@/lib/api';
import { useT } from '@/lib/i18n';

interface TaskInfo {
  id: string;
  label: string;
  description: string;
  status: string;
  created_at: string;
  started_at?: string;
  completed_at?: string;
  progress?: number;
  result?: string;
  error?: string;
}

export function TasksPage() {
  const t = useT();
  const [tasks, setTasks] = useState<TaskInfo[]>([]);
  const [summary, setSummary] = useState({ queued: 0, running: 0, completed: 0, failed: 0 });
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    fetchTasks();
    const interval = setInterval(fetchTasks, 5000);
    return () => clearInterval(interval);
  }, []);

  async function fetchTasks() {
    try {
      const data = await getTasks();
      setTasks(data.tasks || []);
      setSummary({ queued: data.queued, running: data.running, completed: data.completed, failed: data.failed });
    } catch {
      // ignore
    } finally {
      setLoading(false);
    }
  }

  const statusIcon: Record<string, React.ReactNode> = {
    Queued: <Clock size={14} className="text-muted-foreground" />,
    Running: <Loader2 size={14} className="text-rust animate-spin" />,
    Completed: <CheckCircle size={14} className="text-cyber" />,
    Failed: <XCircle size={14} className="text-red-500" />,
  };

  return (
    <div className="flex flex-col h-full">
      <div className="border-b border-border px-6 py-4 flex items-center justify-between">
        <div>
          <h1 className="text-lg font-semibold">{t('tasks.title')}</h1>
          <p className="text-sm text-muted-foreground">
            {summary.running} running · {summary.queued} queued · {summary.completed} completed · {summary.failed} failed
          </p>
        </div>
        <button
          onClick={() => { setLoading(true); fetchTasks(); }}
          className="p-2 rounded-lg hover:bg-accent text-muted-foreground"
        >
          <RefreshCw size={16} className={loading ? 'animate-spin' : ''} />
        </button>
      </div>

      <div className="flex-1 overflow-y-auto p-6">
        {tasks.length === 0 ? (
          <div className="flex flex-col items-center justify-center h-full text-muted-foreground">
            <Clock size={48} className="mb-4 opacity-30" />
            <p className="text-sm">{t('tasks.empty')}</p>
          </div>
        ) : (
          <div className="space-y-2">
            {tasks.map((task) => (
              <div key={task.id} className="border border-border rounded-lg p-4 bg-card">
                <div className="flex items-start gap-3">
                  <div className="mt-0.5">{statusIcon[task.status] || <Clock size={14} />}</div>
                  <div className="flex-1 min-w-0">
                    <div className="flex items-center gap-2">
                      <span className="font-medium text-sm">{task.label}</span>
                      <span className="text-xs text-muted-foreground font-mono">{task.id.slice(0, 8)}</span>
                    </div>
                    {task.description && (
                      <p className="text-xs text-muted-foreground mt-0.5">{task.description}</p>
                    )}
                    {task.progress !== undefined && task.progress > 0 && (
                      <div className="mt-2 h-1.5 bg-muted rounded-full overflow-hidden">
                        <div
                          className="h-full bg-primary rounded-full transition-all"
                          style={{ width: `${Math.min(task.progress, 100)}%` }}
                        />
                      </div>
                    )}
                    {task.error && (
                      <p className="text-xs text-red-500 mt-1 font-mono">{task.error}</p>
                    )}
                    {task.result && (
                      <pre className="text-xs text-muted-foreground mt-1 bg-muted/50 rounded p-2 overflow-x-auto max-h-[100px]">
                        {task.result}
                      </pre>
                    )}
                  </div>
                  <span className={cn(
                    'text-xs px-2 py-0.5 rounded-full',
                    task.status === 'Running' && 'bg-rust/10 text-rust',
                    task.status === 'Completed' && 'bg-cyber/10 text-cyber',
                    task.status === 'Failed' && 'bg-red-500/10 text-red-500',
                    task.status === 'Queued' && 'bg-muted text-muted-foreground',
                  )}>
                    {task.status}
                  </span>
                </div>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
