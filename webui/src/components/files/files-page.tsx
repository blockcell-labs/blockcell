import { useEffect, useState, useRef } from 'react';
import {
  Folder, File, Image, FileText, FileCode, FileAudio, FileVideo,
  Database, Archive, ChevronRight, ArrowLeft, RefreshCw, Loader2,
  Download, Upload, Eye, Home, FolderOpen,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import {
  getFiles, getFileContent, downloadFileUrl, uploadFile,
  type FileEntry, type FileContent,
} from '@/lib/api';
import { useT } from '@/lib/i18n';

function fileIcon(entry: FileEntry) {
  if (entry.is_dir) return <Folder size={18} className="text-amber-500" />;
  switch (entry.type) {
    case 'image': return <Image size={18} className="text-purple-400" />;
    case 'audio': return <FileAudio size={18} className="text-green-400" />;
    case 'video': return <FileVideo size={18} className="text-blue-400" />;
    case 'text': return <FileCode size={18} className="text-cyan-400" />;
    case 'json': return <FileCode size={18} className="text-amber-400" />;
    case 'pdf': return <FileText size={18} className="text-red-400" />;
    case 'office': return <FileText size={18} className="text-blue-500" />;
    case 'archive': return <Archive size={18} className="text-muted-foreground" />;
    case 'database': return <Database size={18} className="text-muted-foreground" />;
    default: return <File size={18} className="text-muted-foreground" />;
  }
}

function formatSize(bytes: number): string {
  if (bytes === 0) return '—';
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
}

export function FilesPage() {
  const t = useT();
  const [currentPath, setCurrentPath] = useState('.');
  const [entries, setEntries] = useState<FileEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [preview, setPreview] = useState<FileContent | null>(null);
  const [previewLoading, setPreviewLoading] = useState(false);
  const [showUpload, setShowUpload] = useState(false);
  const [uploadPath, setUploadPath] = useState('');
  const [uploadContent, setUploadContent] = useState('');
  const fileInputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    fetchFiles(currentPath);
  }, [currentPath]);

  async function fetchFiles(path: string) {
    setLoading(true);
    setPreview(null);
    try {
      const data = await getFiles(path);
      setEntries(data.entries || []);
    } catch {
      setEntries([]);
    } finally {
      setLoading(false);
    }
  }

  function navigateTo(entry: FileEntry) {
    if (entry.is_dir) {
      setCurrentPath(entry.path);
    } else {
      previewFile(entry);
    }
  }

  function goUp() {
    if (currentPath === '.' || currentPath === '') return;
    const parts = currentPath.split('/');
    parts.pop();
    setCurrentPath(parts.length === 0 ? '.' : parts.join('/'));
  }

  function goHome() {
    setCurrentPath('.');
  }

  async function previewFile(entry: FileEntry) {
    setPreviewLoading(true);
    try {
      const data = await getFileContent(entry.path);
      setPreview(data);
    } catch {
      setPreview(null);
    } finally {
      setPreviewLoading(false);
    }
  }

  async function handleUploadText() {
    if (!uploadPath || !uploadContent) return;
    const fullPath = currentPath === '.' ? uploadPath : `${currentPath}/${uploadPath}`;
    try {
      await uploadFile(fullPath, uploadContent);
      setShowUpload(false);
      setUploadPath('');
      setUploadContent('');
      fetchFiles(currentPath);
    } catch {
      // ignore
    }
  }

  async function handleFileInput(e: React.ChangeEvent<HTMLInputElement>) {
    const file = e.target.files?.[0];
    if (!file) return;
    const reader = new FileReader();
    reader.onload = async () => {
      const base64 = (reader.result as string).split(',')[1];
      const fullPath = currentPath === '.' ? file.name : `${currentPath}/${file.name}`;
      try {
        await uploadFile(fullPath, base64, 'base64');
        fetchFiles(currentPath);
      } catch {
        // ignore
      }
    };
    reader.readAsDataURL(file);
    e.target.value = '';
  }

  const breadcrumbs = currentPath === '.'
    ? [{ label: t('files.workspace'), path: '.' }]
    : [
        { label: t('files.workspace'), path: '.' },
        ...currentPath.split('/').map((part, i, arr) => ({
          label: part,
          path: arr.slice(0, i + 1).join('/'),
        })),
      ];

  // Separate media entries for gallery view
  const mediaEntries = entries.filter((e) => e.type === 'image');
  const dirCount = entries.filter((e) => e.is_dir).length;
  const fileCount = entries.filter((e) => !e.is_dir).length;

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="border-b border-border px-6 py-4 flex items-center justify-between">
        <div>
          <h1 className="text-lg font-semibold">{t('files.title')}</h1>
          <p className="text-sm text-muted-foreground">
            {dirCount} {t('files.folders')} · {fileCount} {t('files.fileCount')}
          </p>
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={() => setShowUpload(!showUpload)}
            className="flex items-center gap-1.5 px-3 py-1.5 text-sm rounded-lg bg-primary text-primary-foreground hover:bg-primary/90"
          >
            <Upload size={14} /> {t('common.upload')}
          </button>
          <button
            onClick={() => fetchFiles(currentPath)}
            className="p-2 rounded-lg hover:bg-accent text-muted-foreground"
          >
            <RefreshCw size={16} className={loading ? 'animate-spin' : ''} />
          </button>
        </div>
      </div>

      {/* Breadcrumbs */}
      <div className="border-b border-border px-6 py-2 flex items-center gap-1 text-sm overflow-x-auto">
        <button onClick={goHome} className="p-1 rounded hover:bg-accent text-muted-foreground shrink-0">
          <Home size={14} />
        </button>
        {currentPath !== '.' && (
          <button onClick={goUp} className="p-1 rounded hover:bg-accent text-muted-foreground shrink-0">
            <ArrowLeft size={14} />
          </button>
        )}
        {breadcrumbs.map((bc, i) => (
          <div key={bc.path} className="flex items-center gap-1 shrink-0">
            {i > 0 && <ChevronRight size={12} className="text-muted-foreground" />}
            <button
              onClick={() => setCurrentPath(bc.path)}
              className={cn(
                'px-1.5 py-0.5 rounded hover:bg-accent transition-colors',
                i === breadcrumbs.length - 1 ? 'text-foreground font-medium' : 'text-muted-foreground'
              )}
            >
              {bc.label}
            </button>
          </div>
        ))}
      </div>

      {/* Upload form */}
      {showUpload && (
        <div className="border-b border-border p-4 bg-card/50 space-y-3">
          <div className="flex items-center gap-3">
            <button
              onClick={() => fileInputRef.current?.click()}
              className="flex items-center gap-1.5 px-3 py-1.5 text-sm rounded-lg border border-border hover:bg-accent transition-colors"
            >
              <Upload size={14} /> {t('files.chooseFile')}
            </button>
            <input ref={fileInputRef} type="file" className="hidden" onChange={handleFileInput} />
            <span className="text-xs text-muted-foreground">{t('files.createTextFile')}</span>
          </div>
          <div className="flex items-center gap-3">
            <input
              value={uploadPath}
              onChange={(e) => setUploadPath(e.target.value)}
              placeholder={t('files.filenamePlaceholder')}
              className="px-3 py-1.5 text-sm bg-background border border-border rounded-lg outline-none focus:ring-1 focus:ring-ring flex-1"
            />
            <button
              onClick={handleUploadText}
              disabled={!uploadPath || !uploadContent}
              className="px-4 py-1.5 text-sm rounded-lg bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
            >
              {t('common.create')}
            </button>
          </div>
          <textarea
            value={uploadContent}
            onChange={(e) => setUploadContent(e.target.value)}
            placeholder={t('files.contentPlaceholder')}
            rows={3}
            className="w-full px-3 py-1.5 text-sm bg-background border border-border rounded-lg outline-none focus:ring-1 focus:ring-ring resize-none font-mono"
          />
        </div>
      )}

      {/* Main content: split view */}
      <div className="flex-1 flex overflow-hidden">
        {/* File list */}
        <div className={cn('overflow-y-auto', preview ? 'w-1/2 border-r border-border' : 'w-full')}>
          {loading ? (
            <div className="flex items-center justify-center h-32">
              <Loader2 size={24} className="animate-spin text-muted-foreground" />
            </div>
          ) : entries.length === 0 ? (
            <div className="flex flex-col items-center justify-center h-32 text-muted-foreground">
              <FolderOpen size={32} className="mb-2 opacity-30" />
              <p className="text-sm">{t('files.emptyDir')}</p>
            </div>
          ) : (
            <div className="divide-y divide-border">
              {entries.map((entry) => (
                <div
                  key={entry.path}
                  className="group flex items-center gap-3 px-6 py-2.5 hover:bg-accent/30 cursor-pointer transition-colors"
                  onClick={() => navigateTo(entry)}
                >
                  {fileIcon(entry)}
                  <div className="flex-1 min-w-0">
                    <span className="text-sm truncate block">{entry.name}</span>
                  </div>
                  <span className="text-xs text-muted-foreground shrink-0">
                    {entry.is_dir ? '' : formatSize(entry.size)}
                  </span>
                  <span className="text-xs text-muted-foreground shrink-0 w-36 text-right">
                    {entry.modified ? new Date(entry.modified).toLocaleDateString() : ''}
                  </span>
                  {!entry.is_dir && (
                    <a
                      href={downloadFileUrl(entry.path)}
                      onClick={(e) => e.stopPropagation()}
                      className="opacity-0 group-hover:opacity-100 p-1 rounded hover:bg-accent text-muted-foreground transition-opacity"
                      title={t('common.download')}
                    >
                      <Download size={14} />
                    </a>
                  )}
                </div>
              ))}
            </div>
          )}

          {/* Media gallery */}
          {mediaEntries.length > 0 && (
            <div className="px-6 py-4 border-t border-border">
              <h3 className="text-xs font-medium text-muted-foreground mb-3 uppercase tracking-wider">
                <span className="text-rust">▸</span> {t('files.media')} ({mediaEntries.length})
              </h3>
              <div className="grid grid-cols-3 sm:grid-cols-4 lg:grid-cols-6 gap-2">
                {mediaEntries.map((entry) => (
                  <div
                    key={entry.path}
                    className="aspect-square rounded-lg border border-border overflow-hidden cursor-pointer hover:border-rust/50 transition-colors bg-muted/30 flex items-center justify-center"
                    onClick={() => previewFile(entry)}
                  >
                    <Image size={24} className="text-muted-foreground/30" />
                  </div>
                ))}
              </div>
            </div>
          )}
        </div>

        {/* Preview panel */}
        {preview && (
          <div className="w-1/2 flex flex-col overflow-hidden">
            <div className="border-b border-border px-4 py-2 flex items-center justify-between">
              <div className="flex items-center gap-2 min-w-0">
                <Eye size={14} className="text-muted-foreground shrink-0" />
                <span className="text-sm font-mono truncate">{preview.path.split('/').pop()}</span>
                <span className="text-xs text-muted-foreground shrink-0">{formatSize(preview.size)}</span>
              </div>
              <div className="flex items-center gap-1 shrink-0">
                <a
                  href={downloadFileUrl(preview.path)}
                  className="p-1.5 rounded hover:bg-accent text-muted-foreground"
                  title={t('common.download')}
                >
                  <Download size={14} />
                </a>
                <button
                  onClick={() => setPreview(null)}
                  className="p-1.5 rounded hover:bg-accent text-muted-foreground"
                >
                  ✕
                </button>
              </div>
            </div>
            <div className="flex-1 overflow-auto p-4">
              {previewLoading ? (
                <div className="flex items-center justify-center h-32">
                  <Loader2 size={24} className="animate-spin text-muted-foreground" />
                </div>
              ) : preview.mime_type.startsWith('image/') ? (
                <img
                  src={`data:${preview.mime_type};base64,${preview.content}`}
                  alt={preview.path}
                  className="max-w-full rounded-lg"
                />
              ) : preview.encoding === 'utf-8' ? (
                <pre className="text-xs font-mono whitespace-pre-wrap break-all bg-muted/30 rounded-lg p-3">
                  {preview.content}
                </pre>
              ) : (
                <div className="flex flex-col items-center justify-center h-32 text-muted-foreground">
                  <File size={32} className="mb-2 opacity-30" />
                  <p className="text-sm">{t('files.binaryFile', { mimeType: preview.mime_type })}</p>
                  <a
                    href={downloadFileUrl(preview.path)}
                    className="mt-2 text-xs text-rust hover:text-rust-light underline"
                  >
                    {t('files.downloadToView')}
                  </a>
                </div>
              )}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
