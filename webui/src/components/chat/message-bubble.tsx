import { useState, useMemo } from 'react';
import { User, Bot, ChevronDown, ChevronRight, Clock, Check, AlertCircle } from 'lucide-react';
import { cn } from '@/lib/utils';
import type { UiMessage, ToolCallInfo } from '@/lib/store';
import { MarkdownContent } from './markdown-content';
import { MediaList, extractMediaPaths, isMediaPath } from './media-attachment';

export function MessageBubble({ message }: { message: UiMessage }) {
  const isUser = message.role === 'user';
  const isTool = message.role === 'tool';

  // Collect media: explicit media field + paths detected in content
  const mediaPaths = useMemo(() => {
    const explicit = message.media || [];
    const detected = message.content ? extractMediaPaths(message.content) : [];
    return [...new Set([...explicit, ...detected])];
  }, [message.media, message.content]);

  return (
    <div className={cn('flex gap-3', isUser ? 'flex-row-reverse' : 'flex-row')}>
      {/* Avatar */}
      <div
        className={cn(
          'w-8 h-8 rounded-full flex items-center justify-center shrink-0 border',
          isUser
            ? 'bg-rust/10 border-rust/40 text-rust'
            : 'bg-card border-cyber/30 text-cyber'
        )}
      >
        {isUser ? <User size={16} /> : <Bot size={16} />}
      </div>

      {/* Content */}
      <div className={cn('flex flex-col gap-1 max-w-[80%] min-w-0', isUser ? 'items-end' : 'items-start')}>
        {/* Reasoning (thinking) */}
        {message.reasoning && (
          <CollapsibleSection title="Thinking" defaultOpen={false}>
            <div className="text-xs text-muted-foreground whitespace-pre-wrap">{message.reasoning}</div>
          </CollapsibleSection>
        )}

        {/* Tool calls */}
        {message.toolCalls && message.toolCalls.length > 0 && (
          <div className="w-full space-y-1">
            {message.toolCalls.map((tc) => (
              <ToolCallCard key={tc.id} toolCall={tc} />
            ))}
          </div>
        )}

        {/* Media attachments */}
        {mediaPaths.length > 0 && (
          <MediaList paths={mediaPaths} />
        )}

        {/* Message content */}
        {message.content && (
          <div
            className={cn(
              'rounded-lg px-4 py-2.5 text-sm',
              isUser
                ? 'bg-rust/10 border border-rust/30 text-rust-light rounded-br-sm'
                : 'bg-card border border-cyber/20 rounded-bl-sm'
            )}
          >
            {isUser ? (
              <p className="whitespace-pre-wrap">{message.content}</p>
            ) : (
              <MarkdownContent content={message.content} />
            )}
          </div>
        )}

        {/* Streaming indicator */}
        {message.streaming && (
          <span className="inline-block w-2 h-4 bg-cyber/60 animate-pulse rounded-sm" />
        )}
      </div>
    </div>
  );
}

function ToolCallCard({ toolCall }: { toolCall: ToolCallInfo }) {
  const [isOpen, setIsOpen] = useState(false);

  const statusIcon = {
    running: <Clock size={14} className="text-yellow-500 animate-spin" />,
    done: <Check size={14} className="text-green-500" />,
    error: <AlertCircle size={14} className="text-red-500" />,
  }[toolCall.status];

  return (
    <div className="border border-border rounded-lg overflow-hidden bg-card/50">
      <button
        onClick={() => setIsOpen(!isOpen)}
        className="w-full flex items-center gap-2 px-3 py-2 text-xs hover:bg-accent/50 transition-colors"
      >
        {isOpen ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
        {statusIcon}
        <span className="font-mono font-medium">{toolCall.tool}</span>
        {toolCall.durationMs !== undefined && (
          <span className="text-muted-foreground ml-auto">{toolCall.durationMs}ms</span>
        )}
      </button>
      {isOpen && (
        <div className="border-t border-border px-3 py-2 space-y-2">
          {toolCall.params && (
            <div>
              <span className="text-[10px] uppercase text-muted-foreground font-medium">Parameters</span>
              <pre className="text-xs bg-muted/50 rounded p-2 overflow-x-auto mt-0.5">
                {typeof toolCall.params === 'string'
                  ? toolCall.params
                  : JSON.stringify(toolCall.params, null, 2)}
              </pre>
            </div>
          )}
          {toolCall.result !== undefined && (
            <div>
              <span className="text-[10px] uppercase text-muted-foreground font-medium">Result</span>
              <pre className="text-xs bg-muted/50 rounded p-2 overflow-x-auto mt-0.5 max-h-[200px]">
                {typeof toolCall.result === 'string'
                  ? toolCall.result
                  : JSON.stringify(toolCall.result, null, 2)}
              </pre>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function CollapsibleSection({
  title,
  defaultOpen = false,
  children,
}: {
  title: string;
  defaultOpen?: boolean;
  children: React.ReactNode;
}) {
  const [isOpen, setIsOpen] = useState(defaultOpen);

  return (
    <div className="w-full border border-border rounded-lg overflow-hidden bg-card/50">
      <button
        onClick={() => setIsOpen(!isOpen)}
        className="w-full flex items-center gap-2 px-3 py-1.5 text-xs hover:bg-accent/50 transition-colors text-muted-foreground"
      >
        {isOpen ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
        <span>{title}</span>
      </button>
      {isOpen && <div className="border-t border-border px-3 py-2">{children}</div>}
    </div>
  );
}
