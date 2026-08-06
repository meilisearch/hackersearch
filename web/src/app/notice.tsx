/**
 * Full-width framed message for empty/error states in the results column.
 * Shared by the search results and the thread view.
 */
export function Notice({
  title,
  icon,
  children,
}: {
  title: string;
  icon?: React.ReactNode;
  children?: React.ReactNode;
}) {
  return (
    <div className="mt-6 border bg-card p-6 font-mono text-sm text-muted-foreground">
      <h2 className="mb-2 flex items-center gap-2 font-semibold text-foreground">
        {icon}
        {title}
      </h2>
      {children}
    </div>
  );
}
