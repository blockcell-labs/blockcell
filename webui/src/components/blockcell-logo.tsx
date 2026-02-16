interface BlockcellLogoProps {
  size?: 'xs' | 'sm' | 'md' | 'lg';
  className?: string;
}

export function BlockcellLogo({ size = 'md', className = '' }: BlockcellLogoProps) {
  // Base size reference: md ~ 128px container, svg ~ 100x116
  const dims = { 
    xs: 'w-7 h-7',      // 28px
    sm: 'w-24 h-24',    // 96px
    md: 'w-32 h-32',    // 128px
    lg: 'w-40 h-40'     // 160px
  }[size];

  const scale = { 
    xs: 0.24, 
    sm: 0.75, 
    md: 1, 
    lg: 1.25 
  }[size];

  return (
    <div className={`relative ${dims} flex items-center justify-center ${className}`}>
      <div className="relative w-full h-full animate-[spin_20s_linear_infinite] will-change-transform">
        {/* Background Hexagon (Dark Structure) */}
        <div className="absolute top-1/2 left-1/2 -translate-x-1/2 -translate-y-1/2">
          <svg width={100 * scale} height={116 * scale} viewBox="0 0 100 116" fill="none" className="opacity-50">
            <path d="M50 0L93.3013 25V75L50 100L6.69873 75V25L50 0Z" fill="#0f172a" stroke="#ea580c" strokeWidth="2"/>
          </svg>
        </div>

        {/* Mid Layer Hexagon (Rust Orange - Hardware) */}
        <div className="absolute top-1/2 left-1/2 -translate-x-1/2 -translate-y-1/2 rotate-12">
          <svg width={80 * scale} height={92 * scale} viewBox="0 0 100 116" fill="none" className="opacity-70">
            <path d="M50 0L93.3013 25V75L50 100L6.69873 75V25L50 0Z" stroke="#ea580c" strokeWidth="4" strokeDasharray="10 5"/>
          </svg>
        </div>

        {/* Inner Core (Cyberpunk Green - AI Cell) */}
        <div className="absolute top-1/2 left-1/2 -translate-x-1/2 -translate-y-1/2 animate-pulse">
          <svg width={40 * scale} height={46 * scale} viewBox="0 0 100 116" fill="none">
            <path d="M50 0L93.3013 25V75L50 100L6.69873 75V25L50 0Z" fill="#00ff9d" className="drop-shadow-[0_0_15px_rgba(0,255,157,0.8)]"/>
          </svg>
        </div>

        {/* Floating Satellites (Blocks assembling) */}
        <div className="absolute -top-1 right-1 animate-bounce">
          <svg width={20 * scale} height={24 * scale} viewBox="0 0 100 116">
            <path d="M50 0L93.3013 25V75L50 100L6.69873 75V25L50 0Z" fill="#ea580c" className="opacity-80"/>
          </svg>
        </div>
        <div className="absolute bottom-0 -left-1 animate-bounce [animation-delay:300ms]">
          <svg width={20 * scale} height={24 * scale} viewBox="0 0 100 116">
            <path d="M50 0L93.3013 25V75L50 100L6.69873 75V25L50 0Z" fill="#ea580c" className="opacity-80"/>
          </svg>
        </div>
      </div>
    </div>
  );
}
