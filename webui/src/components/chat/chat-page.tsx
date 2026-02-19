'use client';

import { useRef, useEffect, useState, useCallback } from 'react';
import { Send, Loader2, Paperclip, X, Image as ImageIcon, FileAudio } from 'lucide-react';
import { cn } from '@/lib/utils';
import { useChatStore } from '@/lib/store';
import { wsManager } from '@/lib/ws';
import { getSession, uploadFile, mediaFileUrl } from '@/lib/api';
import { MessageBubble } from './message-bubble';
import { BlockcellLogo } from '../blockcell-logo';
import { useT } from '@/lib/i18n';
import { isMediaPath } from './media-attachment';
import type { UiMessage } from '@/lib/store';

interface PendingFile {
  file: File;
  previewUrl: string;
  type: 'image' | 'audio' | 'video';
}

export function ChatPage() {
  const { messages, sessions, currentSessionId, setMessages, addMessage, isLoading, setLoading } = useChatStore();
  const t = useT();
  const [input, setInput] = useState('');
  const [pendingFiles, setPendingFiles] = useState<PendingFile[]>([]);
  const [uploading, setUploading] = useState(false);
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);

  // Load session history when switching sessions
  useEffect(() => {
    if (currentSessionId) {
      // Check if this session exists in the persisted sessions list
      const isPersistedSession = sessions.some((s) => s.id === currentSessionId);
      if (isPersistedSession) {
        loadSessionHistory(currentSessionId);
      }
    }
  }, [currentSessionId]);

  // Auto-scroll to bottom
  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [messages]);

  // Auto-focus input
  useEffect(() => {
    inputRef.current?.focus();
  }, [currentSessionId]);

  async function loadSessionHistory(sessionId: string) {
    try {
      const data = await getSession(sessionId);
      const uiMessages: UiMessage[] = data.messages
        .filter((m) => {
          // Skip system and tool messages
          if (m.role === 'system' || m.role === 'tool') return false;
          // Skip assistant messages that are only tool calls with no visible content
          if (m.role === 'assistant' && m.tool_calls?.length && !m.content) return false;
          return true;
        })
        .map((m, i) => ({
          id: `hist_${i}`,
          role: m.role,
          content: typeof m.content === 'string' ? m.content : JSON.stringify(m.content),
          reasoning: m.reasoning_content || undefined,
          timestamp: Date.now() - (data.messages.length - i) * 1000,
        }));
      setMessages(uiMessages);
    } catch {
      setMessages([]);
    }
  }

  function handleFileSelect(e: React.ChangeEvent<HTMLInputElement>) {
    const files = e.target.files;
    if (!files) return;
    const newFiles: PendingFile[] = [];
    for (const file of Array.from(files)) {
      const ext = file.name.split('.').pop()?.toLowerCase() || '';
      const type = isMediaPath(`x.${ext}`);
      if (type) {
        newFiles.push({
          file,
          previewUrl: URL.createObjectURL(file),
          type,
        });
      }
    }
    setPendingFiles((prev) => [...prev, ...newFiles]);
    // Reset input so same file can be selected again
    e.target.value = '';
  }

  function removePendingFile(index: number) {
    setPendingFiles((prev) => {
      const removed = prev[index];
      if (removed) URL.revokeObjectURL(removed.previewUrl);
      return prev.filter((_, i) => i !== index);
    });
  }

  async function handleSend() {
    const text = input.trim();
    if ((!text && pendingFiles.length === 0) || isLoading || uploading) return;

    let mediaPaths: string[] = [];

    // Upload pending files first
    if (pendingFiles.length > 0) {
      setUploading(true);
      try {
        for (const pf of pendingFiles) {
          const timestamp = new Date().toISOString().replace(/[:.]/g, '').slice(0, 15);
          const uploadPath = `media/${timestamp}_${pf.file.name}`;
          const reader = new FileReader();
          const b64 = await new Promise<string>((resolve) => {
            reader.onload = () => {
              const result = reader.result as string;
              resolve(result.split(',')[1] || '');
            };
            reader.readAsDataURL(pf.file);
          });
          await uploadFile(uploadPath, b64, 'base64');
          mediaPaths.push(uploadPath);
          URL.revokeObjectURL(pf.previewUrl);
        }
        setPendingFiles([]);
      } catch (err) {
        console.error('File upload failed:', err);
        setUploading(false);
        return;
      }
      setUploading(false);
    }

    const content = text || (mediaPaths.length > 0 ? `[Attached ${mediaPaths.length} file(s)]` : '');

    // Add user message to UI
    addMessage({
      id: `user_${Date.now()}`,
      role: 'user',
      content,
      media: mediaPaths.length > 0 ? mediaPaths : undefined,
      timestamp: Date.now(),
    });

    // Derive chat_id from session — use the session ID directly so the
    // runtime saves history under the same key shown in the sessions list.
    // The session_file() helper on the server normalises ':' → '_', so
    // "ws_1234567890" and "ws:1234567890" both map to the same file.
    const chatId = currentSessionId.replace(/_/g, ':');

    // Send via WebSocket
    wsManager.sendChat(content, chatId, mediaPaths);
    setInput('');
    setLoading(true);
  }

  function handleKeyDown(e: React.KeyboardEvent) {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  }

  return (
    <div className="flex flex-col h-full">
      {/* Messages area */}
      <div className="flex-1 overflow-y-auto px-4 py-6">
        <div className="max-w-3xl mx-auto space-y-4">
          {messages.length === 0 && (
            <div className="flex flex-col items-center justify-center h-full min-h-[50vh] text-muted-foreground">
              <div className="mb-4">
                <BlockcellLogo size="md" />
              </div>
              <h2 className="text-lg font-medium mb-1">Blockcell Agent</h2>
              <p className="text-sm">{t('chat.emptyHint').replace('> ', '')}</p>
            </div>
          )}
          {messages.map((msg) => (
            <MessageBubble key={msg.id} message={msg} />
          ))}
          {isLoading && messages[messages.length - 1]?.role !== 'assistant' && (
            <div className="flex items-center gap-2 text-muted-foreground text-sm">
              <Loader2 size={16} className="animate-spin" />
              <span>{t('chat.thinking')}</span>
            </div>
          )}
          <div ref={messagesEndRef} />
        </div>
      </div>

      {/* Input area */}
      <div className="border-t border-border p-4">
        <div className="max-w-3xl mx-auto">
          {/* Pending file previews */}
          {pendingFiles.length > 0 && (
            <div className="flex gap-2 mb-2 flex-wrap">
              {pendingFiles.map((pf, i) => (
                <div key={i} className="relative group">
                  {pf.type === 'image' ? (
                    <img
                      src={pf.previewUrl}
                      alt={pf.file.name}
                      className="w-16 h-16 object-cover rounded-lg border border-border"
                    />
                  ) : (
                    <div className="w-16 h-16 rounded-lg border border-border bg-muted/50 flex flex-col items-center justify-center">
                      <FileAudio size={20} className="text-muted-foreground" />
                      <span className="text-[8px] text-muted-foreground mt-0.5 truncate max-w-[56px] px-1">
                        {pf.file.name.split('.').pop()}
                      </span>
                    </div>
                  )}
                  <button
                    onClick={() => removePendingFile(i)}
                    className="absolute -top-1.5 -right-1.5 w-4 h-4 rounded-full bg-destructive text-destructive-foreground flex items-center justify-center opacity-0 group-hover:opacity-100 transition-opacity"
                  >
                    <X size={10} />
                  </button>
                </div>
              ))}
            </div>
          )}

          <div className="flex items-end gap-2 bg-card border border-border rounded-xl p-2">
            {/* Hidden file input */}
            <input
              ref={fileInputRef}
              type="file"
              accept="image/*,audio/*,video/*"
              multiple
              onChange={handleFileSelect}
              className="hidden"
            />

            {/* Attachment button */}
            <button
              onClick={() => fileInputRef.current?.click()}
              disabled={isLoading || uploading}
              className="p-2 rounded-lg text-muted-foreground hover:text-foreground transition-colors"
              title="Attach image or audio"
            >
              <Paperclip size={18} />
            </button>

            <textarea
              ref={inputRef}
              value={input}
              onChange={(e) => setInput(e.target.value)}
              onKeyDown={handleKeyDown}
              placeholder={t('chat.inputPlaceholder')}
              className="flex-1 bg-transparent resize-none outline-none text-sm min-h-[40px] max-h-[200px] px-2"
              style={{ paddingTop: 'calc(0.375rem + 3px)', paddingBottom: 'calc(0.375rem - 3px)' }}
              rows={1}
            />
            <button
              onClick={handleSend}
              disabled={(!input.trim() && pendingFiles.length === 0) || isLoading || uploading}
              className={cn(
                'p-2 rounded-lg transition-colors',
                (input.trim() || pendingFiles.length > 0) && !isLoading && !uploading
                  ? 'bg-primary text-primary-foreground hover:bg-primary/90'
                  : 'text-muted-foreground'
              )}
            >
              {uploading ? <Loader2 size={18} className="animate-spin" /> :
               isLoading ? <Loader2 size={18} className="animate-spin" /> : <Send size={18} />}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
