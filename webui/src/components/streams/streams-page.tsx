import { useEffect, useState, useCallback } from 'react';
import {
  Radio, RefreshCw, Loader2, Wifi, WifiOff, ChevronDown, ChevronRight,
  Activity, Clock, AlertCircle, BarChart3,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import { getStreams, getStreamData, type StreamInfo } from '@/lib/api';
import { useT } from '@/lib/i18n';
import {
  LineChart, Line, XAxis, YAxis, CartesianGrid, Tooltip, ResponsiveContainer,
} from 'recharts';

export function StreamsPage() {
  const t = useT();
  const [streams, setStreams] = useState<StreamInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [expandedId, setExpandedId] = useState<string | null>(null);
  const [streamData, setStreamData] = useState<Record<string, any[]>>({});
  const [chartData, setChartData] = useState<Record<string, any[]>>({});

  useEffect(() => {
    fetchStreams();
    const interval = setInterval(fetchStreams, 5000);
    return () => clearInterval(interval);
  }, []);

  // Auto-refresh data for expanded stream
  useEffect(() => {
    if (!expandedId) return;
    fetchStreamData(expandedId);
    const interval = setInterval(() => fetchStreamData(expandedId), 3000);
    return () => clearInterval(interval);
  }, [expandedId]);

  async function fetchStreams() {
    try {
      const data = await getStreams();
      setStreams(data.streams || []);
    } catch {
      // ignore
    } finally {
      setLoading(false);
    }
  }

  async function fetchStreamData(streamId: string) {
    try {
      const data = await getStreamData(streamId, 100);
      const messages = data.messages || [];
      setStreamData((prev) => ({ ...prev, [streamId]: messages }));

      // Try to extract numeric data for charting
      const chartPoints: any[] = [];
      for (const msg of messages) {
        try {
          const parsed = typeof msg.data === 'string' ? JSON.parse(msg.data) : msg.data;
          const value = extractNumericValue(parsed);
          if (value !== null) {
            chartPoints.push({
              time: new Date(msg.timestamp).toLocaleTimeString(),
              value,
              timestamp: msg.timestamp,
            });
          }
        } catch {
          // not JSON, skip charting
        }
      }
      if (chartPoints.length > 0) {
        setChartData((prev) => ({ ...prev, [streamId]: chartPoints }));
      }
    } catch {
      // ignore
    }
  }

  function extractNumericValue(obj: any): number | null {
    if (typeof obj === 'number') return obj;
    if (typeof obj !== 'object' || !obj) return null;
    // Common price fields
    for (const key of ['p', 'price', 'last', 'close', 'c', 'value', 'data', 'result']) {
      if (typeof obj[key] === 'number') return obj[key];
      if (typeof obj[key] === 'string') {
        const n = parseFloat(obj[key]);
        if (!isNaN(n)) return n;
      }
    }
    return null;
  }

  function statusIcon(status: string) {
    switch (status) {
      case 'connected':
        return <Wifi size={14} className="text-cyber" />;
      case 'connecting':
        return <Loader2 size={14} className="text-amber-500 animate-spin" />;
      case 'error':
        return <AlertCircle size={14} className="text-red-500" />;
      default:
        return <WifiOff size={14} className="text-muted-foreground" />;
    }
  }

  function formatTime(ms?: number) {
    if (!ms) return '—';
    return new Date(ms).toLocaleString();
  }

  function formatSize(count: number) {
    if (count >= 1000000) return `${(count / 1000000).toFixed(1)}M`;
    if (count >= 1000) return `${(count / 1000).toFixed(1)}K`;
    return String(count);
  }

  const connectedCount = streams.filter((s) => s.status === 'connected').length;

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="border-b border-border px-6 py-4 flex items-center justify-between">
        <div>
          <h1 className="text-lg font-semibold">{t('streams.title')}</h1>
          <p className="text-sm text-muted-foreground">
            {streams.length} subscriptions · {connectedCount} connected
          </p>
        </div>
        <button
          onClick={() => { setLoading(true); fetchStreams(); }}
          className="p-2 rounded-lg hover:bg-accent text-muted-foreground"
        >
          <RefreshCw size={16} className={loading ? 'animate-spin' : ''} />
        </button>
      </div>

      {/* Content */}
      <div className="flex-1 overflow-y-auto p-6">
        {loading ? (
          <div className="flex items-center justify-center h-32">
            <Loader2 size={24} className="animate-spin text-muted-foreground" />
          </div>
        ) : streams.length === 0 ? (
          <div className="flex flex-col items-center justify-center h-64 text-muted-foreground">
            <Radio size={48} className="mb-4 opacity-30" />
            <p className="text-sm">{t('streams.empty')}</p>
            <p className="text-xs mt-1">Use the <code className="bg-muted px-1.5 py-0.5 rounded">stream_subscribe</code> tool to create subscriptions</p>
          </div>
        ) : (
          <div className="space-y-3">
            {streams.map((stream) => (
              <div key={stream.stream_id} className="border border-border rounded-lg bg-card overflow-hidden">
                {/* Stream header */}
                <div
                  className="flex items-center gap-3 px-4 py-3 cursor-pointer hover:bg-accent/30 transition-colors"
                  onClick={() => setExpandedId(expandedId === stream.stream_id ? null : stream.stream_id)}
                >
                  {expandedId === stream.stream_id ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
                  {statusIcon(stream.status)}
                  <div className="flex-1 min-w-0">
                    <div className="flex items-center gap-2">
                      <span className="font-medium text-sm font-mono truncate">{stream.stream_id}</span>
                      <span className={cn(
                        'text-[10px] px-1.5 py-0.5 rounded-full',
                        stream.status === 'connected' ? 'bg-cyber/10 text-cyber' :
                        stream.status === 'error' ? 'bg-red-500/10 text-red-500' :
                        'bg-muted text-muted-foreground'
                      )}>
                        {stream.status}
                      </span>
                      <span className="text-[10px] px-1.5 py-0.5 rounded bg-muted text-muted-foreground">
                        {stream.protocol}
                      </span>
                    </div>
                    <p className="text-xs text-muted-foreground truncate mt-0.5">{stream.url}</p>
                  </div>
                  <div className="flex items-center gap-4 text-xs text-muted-foreground shrink-0">
                    <div className="flex items-center gap-1">
                      <Activity size={12} />
                      <span>{formatSize(stream.message_count)} msgs</span>
                    </div>
                    <div className="flex items-center gap-1">
                      <BarChart3 size={12} />
                      <span>{stream.buffered} buffered</span>
                    </div>
                  </div>
                </div>

                {/* Expanded content */}
                {expandedId === stream.stream_id && (
                  <div className="border-t border-border">
                    {/* Chart */}
                    {chartData[stream.stream_id]?.length > 1 && (
                      <div className="px-4 pt-3 pb-1">
                        <h3 className="text-xs font-medium text-muted-foreground mb-2 uppercase tracking-wider">
                          <span className="text-rust">▸</span> Live Chart
                        </h3>
                        <div className="h-48 w-full">
                          <ResponsiveContainer width="100%" height="100%">
                            <LineChart data={chartData[stream.stream_id]}>
                              <CartesianGrid strokeDasharray="3 3" stroke="hsl(217 33% 17%)" />
                              <XAxis
                                dataKey="time"
                                tick={{ fontSize: 10, fill: 'hsl(215 20% 65%)' }}
                                interval="preserveStartEnd"
                              />
                              <YAxis
                                tick={{ fontSize: 10, fill: 'hsl(215 20% 65%)' }}
                                domain={['auto', 'auto']}
                              />
                              <Tooltip
                                contentStyle={{
                                  backgroundColor: 'hsl(222 47% 11%)',
                                  border: '1px solid hsl(217 33% 17%)',
                                  borderRadius: '8px',
                                  fontSize: '12px',
                                }}
                              />
                              <Line
                                type="monotone"
                                dataKey="value"
                                stroke="#ea580c"
                                strokeWidth={2}
                                dot={false}
                                activeDot={{ r: 4, fill: '#ea580c' }}
                              />
                            </LineChart>
                          </ResponsiveContainer>
                        </div>
                      </div>
                    )}

                    {/* Details */}
                    <div className="px-4 py-3 space-y-2">
                      <div className="grid grid-cols-2 gap-x-6 gap-y-1 text-xs">
                        <div><span className="text-muted-foreground">Created:</span> {formatTime(stream.created_at)}</div>
                        <div><span className="text-muted-foreground">Last message:</span> {formatTime(stream.last_message_at)}</div>
                        <div><span className="text-muted-foreground">Auto-restore:</span> {stream.auto_restore ? 'Yes' : 'No'}</div>
                        <div><span className="text-muted-foreground">Reconnects:</span> {stream.reconnect_count}</div>
                      </div>
                      {stream.error && (
                        <div className="text-xs text-red-500">
                          <AlertCircle size={12} className="inline mr-1" />
                          {stream.error}
                        </div>
                      )}

                      {/* Recent messages */}
                      {streamData[stream.stream_id]?.length > 0 && (
                        <div>
                          <h3 className="text-xs font-medium text-muted-foreground mb-1.5 uppercase tracking-wider">
                            <span className="text-cyber">▸</span> Recent Messages ({streamData[stream.stream_id].length})
                          </h3>
                          <div className="max-h-48 overflow-y-auto space-y-1">
                            {streamData[stream.stream_id].slice(-20).reverse().map((msg: any, i: number) => (
                              <div key={i} className="bg-muted/50 rounded px-2 py-1 text-xs font-mono overflow-x-auto">
                                <span className="text-muted-foreground mr-2">
                                  {new Date(msg.timestamp).toLocaleTimeString()}
                                </span>
                                <span className="break-all">
                                  {typeof msg.data === 'string'
                                    ? msg.data.length > 200 ? msg.data.slice(0, 200) + '...' : msg.data
                                    : JSON.stringify(msg.data).slice(0, 200)}
                                </span>
                              </div>
                            ))}
                          </div>
                        </div>
                      )}
                    </div>
                  </div>
                )}
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
