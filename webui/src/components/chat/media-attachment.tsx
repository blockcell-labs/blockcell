import { useState } from 'react';
import { Image, Volume2, Download, Maximize2, X, FileAudio } from 'lucide-react';
import { cn } from '@/lib/utils';
import { mediaFileUrl, downloadFileUrl } from '@/lib/api';

const IMAGE_EXTS = ['png', 'jpg', 'jpeg', 'gif', 'webp', 'bmp', 'svg', 'ico', 'heic', 'heif', 'tiff', 'tif'];
const AUDIO_EXTS = ['mp3', 'wav', 'm4a', 'aac', 'ogg', 'oga', 'flac', 'opus', 'weba'];
const VIDEO_EXTS = ['mp4', 'webm', 'mkv', 'mov'];

function getExt(path: string): string {
  const dot = path.lastIndexOf('.');
  return dot >= 0 ? path.slice(dot + 1).toLowerCase() : '';
}

export function isMediaPath(path: string): 'image' | 'audio' | 'video' | null {
  const ext = getExt(path);
  if (IMAGE_EXTS.includes(ext)) return 'image';
  if (AUDIO_EXTS.includes(ext)) return 'audio';
  if (VIDEO_EXTS.includes(ext)) return 'video';
  return null;
}

/** Extract file paths from message text that look like media files */
export function extractMediaPaths(text: string): string[] {
  const paths: string[] = [];
  const exts = 'png|jpg|jpeg|gif|webp|bmp|svg|mp3|wav|m4a|aac|ogg|flac|opus|mp4|webm|mov';

  // Match absolute paths: /path/to/file.ext
  // Broad context: after whitespace, colon, backtick, paren, quote, line start, etc.
  const absRegex = new RegExp(`((?:/[\\w.\\-@]+)+/[\\w.\\-@]+\\.(?:${exts}))`, 'gi');
  let match;
  while ((match = absRegex.exec(text)) !== null) {
    paths.push(match[1]);
  }

  // Match workspace-relative paths like media/xxx.png, photo_xxx.jpg
  // Must NOT start with / (already caught above) or http
  const relRegex = new RegExp(`(?:^|[\\s:(\`"'])([\\w][\\w.\\-]*/[\\w.\\-/]*\\.(?:${exts}))`, 'gim');
  while ((match = relRegex.exec(text)) !== null) {
    if (!match[1].startsWith('/') && !match[1].startsWith('http')) {
      paths.push(match[1]);
    }
  }

  // Match markdown image syntax: ![alt](path)
  const mdRegex = /!\[[^\]]*\]\(([^)]+\.(?:png|jpg|jpeg|gif|webp|bmp|svg))\)/gi;
  while ((match = mdRegex.exec(text)) !== null) {
    paths.push(match[1]);
  }

  return [...new Set(paths)];
}

export function MediaAttachment({ path }: { path: string }) {
  const type = isMediaPath(path);
  const url = mediaFileUrl(path);
  const dlUrl = downloadFileUrl(path);
  const filename = path.split('/').pop() || path;

  if (type === 'image') {
    return <ImageAttachment url={url} dlUrl={dlUrl} filename={filename} />;
  }
  if (type === 'audio') {
    return <AudioAttachment url={url} dlUrl={dlUrl} filename={filename} />;
  }
  if (type === 'video') {
    return <VideoAttachment url={url} dlUrl={dlUrl} filename={filename} />;
  }
  return null;
}

function ImageAttachment({ url, dlUrl, filename }: { url: string; dlUrl: string; filename: string }) {
  const [expanded, setExpanded] = useState(false);
  const [error, setError] = useState(false);

  if (error) {
    return (
      <div className="flex items-center gap-2 text-xs text-muted-foreground bg-muted/50 rounded-lg px-3 py-2">
        <Image size={14} />
        <span className="truncate">{filename}</span>
        <a href={dlUrl} download className="ml-auto hover:text-foreground"><Download size={14} /></a>
      </div>
    );
  }

  return (
    <>
      <div className="relative group rounded-lg overflow-hidden border border-border bg-card/50 max-w-sm">
        <img
          src={url}
          alt={filename}
          className="max-w-full max-h-[300px] object-contain cursor-pointer"
          onClick={() => setExpanded(true)}
          onError={() => setError(true)}
          loading="lazy"
        />
        <div className="absolute top-1 right-1 flex gap-1 opacity-0 group-hover:opacity-100 transition-opacity">
          <button
            onClick={() => setExpanded(true)}
            className="p-1 rounded bg-black/60 text-white hover:bg-black/80"
          >
            <Maximize2 size={12} />
          </button>
          <a href={dlUrl} download className="p-1 rounded bg-black/60 text-white hover:bg-black/80">
            <Download size={12} />
          </a>
        </div>
        <div className="px-2 py-1 text-[10px] text-muted-foreground truncate">{filename}</div>
      </div>

      {/* Lightbox */}
      {expanded && (
        <div
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/80 backdrop-blur-sm"
          onClick={() => setExpanded(false)}
        >
          <button
            className="absolute top-4 right-4 p-2 rounded-full bg-white/10 text-white hover:bg-white/20"
            onClick={() => setExpanded(false)}
          >
            <X size={20} />
          </button>
          <img
            src={url}
            alt={filename}
            className="max-w-[90vw] max-h-[90vh] object-contain rounded-lg shadow-2xl"
            onClick={(e) => e.stopPropagation()}
          />
        </div>
      )}
    </>
  );
}

function AudioAttachment({ url, dlUrl, filename }: { url: string; dlUrl: string; filename: string }) {
  return (
    <div className="flex flex-col gap-1.5 rounded-lg border border-border bg-card/50 p-3 max-w-sm">
      <div className="flex items-center gap-2 text-xs">
        <FileAudio size={14} className="text-cyber shrink-0" />
        <span className="truncate font-medium">{filename}</span>
        <a href={dlUrl} download className="ml-auto text-muted-foreground hover:text-foreground">
          <Download size={14} />
        </a>
      </div>
      <audio controls preload="metadata" className="w-full h-8 [&::-webkit-media-controls-panel]:bg-transparent">
        <source src={url} />
      </audio>
    </div>
  );
}

function VideoAttachment({ url, dlUrl, filename }: { url: string; dlUrl: string; filename: string }) {
  return (
    <div className="flex flex-col gap-1.5 rounded-lg border border-border bg-card/50 overflow-hidden max-w-md">
      <video controls preload="metadata" className="max-w-full max-h-[300px]">
        <source src={url} />
      </video>
      <div className="flex items-center gap-2 text-xs px-2 pb-2">
        <Volume2 size={14} className="text-cyber shrink-0" />
        <span className="truncate">{filename}</span>
        <a href={dlUrl} download className="ml-auto text-muted-foreground hover:text-foreground">
          <Download size={14} />
        </a>
      </div>
    </div>
  );
}

/** Renders a list of media attachments */
export function MediaList({ paths }: { paths: string[] }) {
  if (!paths.length) return null;
  return (
    <div className="flex flex-col gap-2">
      {paths.map((p) => (
        <MediaAttachment key={p} path={p} />
      ))}
    </div>
  );
}
