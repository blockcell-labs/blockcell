'use client';

import { useRef, useEffect, useState } from 'react';
import { Send, Loader2, Paperclip, X, FileAudio, Upload, Square } from 'lucide-react';
import { cn } from '@/lib/utils';
import { useAgentStore, useChatStore } from '@/lib/store';
import { wsManager } from '@/lib/ws';
import { getSession, uploadFile } from '@/lib/api';
import { MessageBubble } from './message-bubble';
import { BlockcellLogo } from '../blockcell-logo';
import { CommandPicker, CommandItem } from './command-picker';
import { useT } from '@/lib/i18n';
import { isMediaPath } from './media-attachment';
import type { UiMessage } from '@/lib/store';

interface PendingFile {
  file: File;
  previewUrl: string;
  type: 'image' | 'audio' | 'video';
}

export function ChatPage() {
  const messages = useChatStore((s) => s.messages);
  const sessions = useChatStore((s) => s.sessions);
  const currentSessionId = useChatStore((s) => s.currentSessionId);
  const setMessages = useChatStore((s) => s.setMessages);
  const addMessage = useChatStore((s) => s.addMessage);
  const isLoading = useChatStore((s) => s.isLoading);
  const setLoading = useChatStore((s) => s.setLoading);
  const selectedAgentId = useAgentStore((s) => s.selectedAgentId);
  const t = useT();
  const [input, setInput] = useState('');
  const [isDragOver, setIsDragOver] = useState(false);
  const [pendingFiles, setPendingFiles] = useState<PendingFile[]>([]);
  const [uploading, setUploading] = useState(false);
  const [isCancelling, setIsCancelling] = useState(false);
  const [showCommandPicker, setShowCommandPicker] = useState(false);
  const [commandQuery, setCommandQuery] = useState('');
  const hasMessages = messages.length > 0;
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const inputContainerRef = useRef<HTMLDivElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const selectedAgentRef = useRef(selectedAgentId);
  const currentSessionRef = useRef(currentSessionId);

  useEffect(() => {
    selectedAgentRef.current = selectedAgentId;
  }, [selectedAgentId]);

  useEffect(() => {
    currentSessionRef.current = currentSessionId;
  }, [currentSessionId]);

  // Load session history when switching sessions
  useEffect(() => {
    if (currentSessionId) {
      const isPersistedSession = sessions.some((s) => s.id === currentSessionId);
      if (isPersistedSession) {
        const currentMessages = useChatStore.getState().messages;
        const hasOnlyOptimisticMessages = currentMessages.length > 0 && currentMessages.every((m) => m.id.startsWith('user_') || m.id.startsWith('msg_'));
        if (!hasOnlyOptimisticMessages) {
          loadSessionHistory(currentSessionId, selectedAgentId);
        }
      } else if (useChatStore.getState().messages.length > 0) {
        setMessages([]);
      }
    } else if (useChatStore.getState().messages.length > 0) {
      setMessages([]);
    }
  }, [currentSessionId, selectedAgentId, sessions, setMessages]);

  // Auto-scroll to bottom
  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: 'auto', block: 'end' });
  }, [messages]);

  useEffect(() => {
    const highlighted = document.querySelector('[data-highlighted-message="true"]');
    if (highlighted) {
      highlighted.scrollIntoView({ behavior: 'smooth', block: 'center' });
    }
  }, [messages, currentSessionId]);

  // Auto-focus input
  useEffect(() => {
    inputRef.current?.focus();
  }, [currentSessionId]);

  useEffect(() => {
    if (!isLoading) {
      setIsCancelling(false);
    }
  }, [isLoading]);

  async function loadSessionHistory(sessionId: string, agentId: string) {
    try {
      const data = await getSession(sessionId, agentId);
      if (selectedAgentRef.current !== agentId || currentSessionRef.current !== sessionId) {
        return;
      }
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
      const currentState = useChatStore.getState();
      if (currentState.currentSessionId !== sessionId) {
        return;
      }
      if (currentState.messages.length > 0) {
        return;
      }
      setMessages(uiMessages);
    } catch {
      const currentState = useChatStore.getState();
      if (
        selectedAgentRef.current === agentId &&
        currentSessionRef.current === sessionId &&
        currentState.currentSessionId === sessionId &&
        currentState.messages.length === 0
      ) {
        setMessages([]);
      }
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

  function handleDragOver(e: React.DragEvent) {
    e.preventDefault();
    setIsDragOver(true);
  }

  function handleDragLeave(e: React.DragEvent) {
    if (!e.currentTarget.contains(e.relatedTarget as Node)) {
      setIsDragOver(false);
    }
  }

  function handleDrop(e: React.DragEvent) {
    e.preventDefault();
    setIsDragOver(false);
    const files = Array.from(e.dataTransfer.files);
    const newFiles: PendingFile[] = [];
    for (const file of files) {
      const ext = file.name.split('.').pop()?.toLowerCase() || '';
      const type = isMediaPath(`x.${ext}`);
      if (type) {
        newFiles.push({ file, previewUrl: URL.createObjectURL(file), type });
      }
    }
    if (newFiles.length > 0) {
      setPendingFiles((prev) => [...prev, ...newFiles]);
    }
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
          await uploadFile(uploadPath, b64, 'base64', selectedAgentId);
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

    const hasPersistedSession = !!currentSessionId && sessions.some((s) => s.id === currentSessionId);
    const chatId = hasPersistedSession ? currentSessionId.replace(/_/g, ':') : undefined;

    // Send via WebSocket
    wsManager.sendChat(content, chatId, mediaPaths, selectedAgentId);
    setInput('');
    setLoading(true);
  }

  function handleCancel() {
    if (!isLoading || isCancelling) return;
    if (!currentSessionId) return;
    const chatId = currentSessionId.replace(/_/g, ':');
    wsManager.sendCancel(chatId, selectedAgentId);
    setIsCancelling(true);
  }

  function handleKeyDown(e: React.KeyboardEvent) {
    // Handle command picker navigation
    if (showCommandPicker) {
      if (e.key === 'ArrowDown' || e.key === 'ArrowUp' || e.key === 'Tab' || e.key === 'Enter') {
        // Let CommandPicker handle these
        return;
      }
      if (e.key === 'Escape') {
        e.preventDefault();
        setShowCommandPicker(false);
        setCommandQuery('');
        return;
      }
    }

    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  }

  function handleInputChange(e: React.ChangeEvent<HTMLTextAreaElement>) {
    const value = e.target.value;
    setInput(value);

    // Detect "/" at start of line or after space
    const cursorPos = e.target.selectionStart;
    const textBeforeCursor = value.slice(0, cursorPos);
    const lastNewline = textBeforeCursor.lastIndexOf('\n');
    const textAfterLastNewline = textBeforeCursor.slice(lastNewline + 1);

    // Find the last '/' in the current line
    const lastSlash = textAfterLastNewline.lastIndexOf('/');
    if (lastSlash !== -1) {
      // Check if '/' is at start or after a space
      const isAtStart = lastSlash === 0;
      const isAfterSpace = lastSlash > 0 && textAfterLastNewline[lastSlash - 1] === ' ';

      if (isAtStart || isAfterSpace) {
        const afterSlash = textAfterLastNewline.slice(lastSlash + 1);
        // Check if there's no space after '/' (still typing command)
        if (!afterSlash.includes(' ')) {
          setShowCommandPicker(true);
          setCommandQuery(afterSlash);
          return;
        }
      }
    }

    setShowCommandPicker(false);
    setCommandQuery('');
  }

  function handleCommandSelect(item: CommandItem) {
    // Insert the command name at cursor position
    const cursorPos = inputRef.current?.selectionStart || 0;
    const textBeforeCursor = input.slice(0, cursorPos);
    const textAfterCursor = input.slice(cursorPos);

    // Find the start of the "/" command (use lastIndexOf to find the last '/')
    const lastNewline = textBeforeCursor.lastIndexOf('\n');
    const slashPos = textBeforeCursor.lastIndexOf('/');

    if (slashPos !== -1 && slashPos > lastNewline) {
      // Replace from "/" to cursor with the command name
      const newText = input.slice(0, slashPos) + '/' + item.name + ' ' + textAfterCursor;
      setInput(newText);
      setShowCommandPicker(false);
      setCommandQuery('');

      // Focus input and move cursor after the inserted command
      setTimeout(() => {
        inputRef.current?.focus();
        const newCursorPos = slashPos + 1 + item.name.length + 1;
        inputRef.current?.setSelectionRange(newCursorPos, newCursorPos);
      }, 0);
    }
  }

  return (
    <div
      className="flex flex-col h-full relative"
      onDragOver={handleDragOver}
      onDragLeave={handleDragLeave}
      onDrop={handleDrop}
    >
      {/* Drag overlay */}
      {isDragOver && (
        <div className="absolute inset-0 z-30 flex flex-col items-center justify-center bg-primary/10 border-2 border-dashed border-primary/50 rounded-none pointer-events-none">
          <Upload size={36} className="text-primary mb-3" />
          <p className="text-primary font-medium">{t('chat.dropFiles')}</p>
        </div>
      )}
      {/* Messages area */}
      <div className="flex-1 overflow-y-auto px-4 py-6">
        <div className="max-w-3xl mx-auto space-y-4">
          {!hasMessages && (
            <div className="flex flex-col items-center pt-8 pb-4 text-muted-foreground">
              <div className="mb-3">
                <BlockcellLogo size="md" />
              </div>
              <h2 className="text-base font-semibold mb-1 text-foreground">Blockcell</h2>
              <p className="text-sm">{t('chat.emptyHint')}</p>
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

          <div ref={inputContainerRef} className="flex items-end gap-2 bg-card border border-border rounded-xl p-2 relative">
            {/* Command Picker */}
            <CommandPicker
              open={showCommandPicker}
              query={commandQuery}
              onSelect={handleCommandSelect}
              onClose={() => {
                setShowCommandPicker(false);
                setCommandQuery('');
              }}
              containerRef={inputContainerRef}
            />

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
              onChange={handleInputChange}
              onKeyDown={handleKeyDown}
              placeholder={t('chat.inputPlaceholder')}
              className="flex-1 bg-transparent resize-none outline-none text-sm min-h-[40px] max-h-[200px] px-2"
              style={{ paddingTop: 'calc(0.375rem + 3px)', paddingBottom: 'calc(0.375rem - 3px)' }}
              rows={1}
            />
            <button
              onClick={isLoading ? handleCancel : handleSend}
              disabled={uploading || (!isLoading && !input.trim() && pendingFiles.length === 0) || (isLoading && isCancelling)}
              className={cn(
                'p-2 rounded-lg transition-colors',
                isLoading
                  ? 'bg-destructive text-destructive-foreground hover:bg-destructive/90'
                  : (input.trim() || pendingFiles.length > 0) && !uploading
                  ? 'bg-primary text-primary-foreground hover:bg-primary/90'
                  : 'text-muted-foreground'
              )}
            >
              {uploading ? <Loader2 size={18} className="animate-spin" /> :
               isLoading ? (isCancelling ? <Loader2 size={18} className="animate-spin" /> : <Square size={18} />) : <Send size={18} />}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
