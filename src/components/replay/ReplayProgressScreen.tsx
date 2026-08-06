type ReplayProgressScreenProps = {
  playbookName: string;
};

export function ReplayProgressScreen({ playbookName }: ReplayProgressScreenProps) {
  return (
    <main className="flex min-h-svh flex-col items-center justify-center gap-4 p-8">
      <h1 className="text-xl font-semibold tracking-tight">Replaying playbook</h1>
      <p className="text-muted-foreground max-w-md text-center text-sm">
        Running &ldquo;{playbookName}&rdquo; — real automation is in progress.
        This can take a while depending on step count and target apps.
      </p>
      <p className="text-muted-foreground animate-pulse text-sm">Please wait…</p>
    </main>
  );
}
