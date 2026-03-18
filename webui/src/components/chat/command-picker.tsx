'use client';

import { useState, useEffect, useRef, useCallback, useMemo } from 'react';
import { createPortal } from 'react-dom';
import { cn } from '@/lib/utils';
import { getTools, getSkills } from '@/lib/api';
import { useT } from '@/lib/i18n';

export interface CommandItem {
  name: string;
  description: string;
  kind: 'tool' | 'skill';
  source?: string;
}

interface CommandPickerProps {
  open: boolean;
  query: string;
  onSelect: (item: CommandItem) => void;
  onClose: () => void;
  containerRef: React.RefObject<HTMLDivElement>;
}

export function CommandPicker({ open, query, onSelect, onClose, containerRef }: CommandPickerProps) {
  const [items, setItems] = useState<CommandItem[]>([]);
  const [loading, setLoading] = useState(false);
  const [selectedIndex, setSelectedIndex] = useState(0);
  const [position, setPosition] = useState({ top: 0, left: 0, width: 0 });
  const listRef = useRef<HTMLDivElement>(null);
  const t = useT();

  // Load tools and skills
  useEffect(() => {
    if (!open) return;

    async function loadItems() {
      setLoading(true);
      try {
        const [toolsData, skillsData] = await Promise.all([
          getTools().catch(() => ({ tools: [], count: 0 })),
          getSkills().catch(() => ({ skills: [], count: 0 })),
        ]);

        const tools: CommandItem[] = (toolsData.tools || []).map((t: any) => ({
          name: t.name || t,
          description: t.description || '',
          kind: 'tool' as const,
          source: t.source,
        }));

        const skills: CommandItem[] = (skillsData.skills || []).map((s: any) => ({
          name: s.name || s,
          description: s.meta?.description || s.description || '',
          kind: 'skill' as const,
          source: s.source,
        }));

        // Tools first, then skills, both sorted alphabetically
        const allItems = [
          ...tools.sort((a, b) => a.name.localeCompare(b.name)),
          ...skills.sort((a, b) => a.name.localeCompare(b.name)),
        ];

        setItems(allItems);
      } catch (err) {
        console.error('Failed to load commands:', err);
        setItems([]);
      } finally {
        setLoading(false);
      }
    }

    loadItems();
  }, [open]);

  // Update position when container changes
  useEffect(() => {
    if (!open || !containerRef.current) return;

    const updatePosition = () => {
      if (containerRef.current) {
        const rect = containerRef.current.getBoundingClientRect();
        setPosition({
          top: rect.top - 4, // 4px gap
          left: rect.left,
          width: rect.width,
        });
      }
    };

    updatePosition();
    window.addEventListener('resize', updatePosition);
    window.addEventListener('scroll', updatePosition, true);

    return () => {
      window.removeEventListener('resize', updatePosition);
      window.removeEventListener('scroll', updatePosition, true);
    };
  }, [open, containerRef]);

  // Filter items based on query with smart sorting
  const filteredItems = useMemo(() => {
    if (!query) return items;
    const q = query.toLowerCase();

    // Score each item: name starts with query = 3, name contains query = 2, description contains query = 1
    const scored = items
      .map((item) => {
        const nameLower = item.name.toLowerCase();
        const descLower = item.description.toLowerCase();
        let score = 0;
        if (nameLower.startsWith(q)) score = 3;
        else if (nameLower.includes(q)) score = 2;
        else if (descLower.includes(q)) score = 1;
        return { item, score };
      })
      .filter(({ score }) => score > 0)
      .sort((a, b) => {
        // Sort by score first, then by name
        if (b.score !== a.score) return b.score - a.score;
        return a.item.name.localeCompare(b.item.name);
      })
      .map(({ item }) => item);

    return scored;
  }, [items, query]);

  // Reset selection when filtered items change
  useEffect(() => {
    setSelectedIndex(0);
  }, [filteredItems.length]);

  // Keyboard navigation
  const handleKeyDown = useCallback(
    (e: KeyboardEvent) => {
      if (!open) return;

      switch (e.key) {
        case 'ArrowDown':
          e.preventDefault();
          setSelectedIndex((prev) =>
            prev < filteredItems.length - 1 ? prev + 1 : 0
          );
          break;
        case 'ArrowUp':
          e.preventDefault();
          setSelectedIndex((prev) =>
            prev > 0 ? prev - 1 : filteredItems.length - 1
          );
          break;
        case 'Tab':
          e.preventDefault();
          if (filteredItems[selectedIndex]) {
            onSelect(filteredItems[selectedIndex]);
          }
          break;
        case 'Enter':
          e.preventDefault();
          if (filteredItems[selectedIndex]) {
            onSelect(filteredItems[selectedIndex]);
          }
          break;
        case 'Escape':
          e.preventDefault();
          onClose();
          break;
      }
    },
    [open, filteredItems, selectedIndex, onSelect, onClose]
  );

  useEffect(() => {
    if (open) {
      window.addEventListener('keydown', handleKeyDown);
    }
    return () => {
      window.removeEventListener('keydown', handleKeyDown);
    };
  }, [open, handleKeyDown]);

  // Scroll selected item into view
  useEffect(() => {
    if (listRef.current && filteredItems.length > 0) {
      const selectedEl = listRef.current.querySelector(
        `[data-index="${selectedIndex}"]`
      );
      if (selectedEl) {
        selectedEl.scrollIntoView({ block: 'nearest' });
      }
    }
  }, [selectedIndex, filteredItems.length]);

  if (!open || typeof document === 'undefined') return null;

  return createPortal(
    <div
      className="fixed"
      style={{
        top: position.top,
        left: position.left,
        width: position.width,
        transform: 'translateY(-100%)',
        zIndex: 99999,
      }}
    >
      <div className="bg-background border border-border rounded-lg shadow-xl overflow-hidden max-h-64">
        {/* Loading state */}
        {loading && (
          <div className="flex items-center justify-center py-4">
            <div className="w-5 h-5 border-2 border-muted-foreground/30 border-t-[hsl(var(--brand-green))] rounded-full animate-spin" />
          </div>
        )}

        {/* Empty state */}
        {!loading && filteredItems.length === 0 && (
          <div className="flex items-center justify-center py-4 text-muted-foreground">
            <span className="text-sm">{t('commandPicker.noResults')}</span>
          </div>
        )}

        {/* Items list */}
        {!loading && filteredItems.length > 0 && (
          <div
            ref={listRef}
            className="overflow-y-auto max-h-56"
          >
            {filteredItems.map((item, index) => (
              <button
                key={`${item.kind}-${item.name}`}
                data-index={index}
                onClick={() => onSelect(item)}
                className={cn(
                  'w-full flex items-center gap-3 px-3 py-2 text-left transition-colors',
                  index === selectedIndex
                    ? 'bg-[hsl(var(--brand-green)/0.10)]'
                    : 'hover:bg-muted/50'
                )}
              >
                {/* Icon */}
                <span className="shrink-0">
                  {item.kind === 'tool' ? '🔧' : '✨'}
                </span>

                {/* Content */}
                <div className="flex-1 min-w-0">
                  <span className="text-sm font-medium">
                    {item.name}
                  </span>
                  {item.description && (
                    <span className="text-xs text-muted-foreground ml-2">
                      {item.description.length > 30 ? item.description.slice(0, 30) + '...' : item.description}
                    </span>
                  )}
                </div>
              </button>
            ))}
          </div>
        )}

        {/* Footer hint */}
        {!loading && filteredItems.length > 0 && (
          <div className="flex items-center justify-between px-3 py-1 border-t border-border/50 bg-muted/20 text-xs text-muted-foreground">
            <span>
              {t('commandPicker.count', { n: filteredItems.length })}
            </span>
            <span className="flex items-center gap-2">
              <kbd className="px-1 rounded bg-muted">↑↓</kbd>
              <span>{t('commandPicker.navigate')}</span>
              <kbd className="px-1 rounded bg-muted">Tab</kbd>
              <span>{t('commandPicker.insert')}</span>
            </span>
          </div>
        )}
      </div>
    </div>,
    document.body
  );
}