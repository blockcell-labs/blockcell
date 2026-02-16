export function ThemeProvider({ children }: { children: React.ReactNode }) {
  // Theme is applied synchronously in store.ts on module load,
  // so no useEffect needed here. This wrapper is kept for structural consistency.
  return <>{children}</>;
}
