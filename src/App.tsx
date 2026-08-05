import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Button } from "@/components/ui/button";
import {
  isProofWindow,
  isTauriRuntime,
  openProofWindow,
} from "@/lib/windows";
import { useWindowLabel } from "@/hooks/useWindowLabel";

function ProofWindowView() {
  return (
    <main className="flex min-h-svh flex-col items-center justify-center gap-2 p-8">
      <h1 className="text-lg font-semibold tracking-tight">Proof Window</h1>
      <p className="text-muted-foreground text-sm">
        Native secondary window — multi-window capability confirmed.
      </p>
    </main>
  );
}

function MainWindowView() {
  const { isSuccess, isError } = useQuery({
    queryKey: ["foundation-health"],
    queryFn: async () => true,
  });
  const [windowError, setWindowError] = useState<string | null>(null);
  const [isOpeningWindow, setIsOpeningWindow] = useState(false);

  async function handleOpenProofWindow() {
    if (!isTauriRuntime()) {
      setWindowError("Multi-window proof requires the Tauri desktop runtime.");
      return;
    }

    setWindowError(null);
    setIsOpeningWindow(true);

    try {
      await openProofWindow();
    } catch (error) {
      setWindowError(
        error instanceof Error ? error.message : "Failed to open proof window.",
      );
    } finally {
      setIsOpeningWindow(false);
    }
  }

  return (
    <main className="flex min-h-svh flex-col items-center justify-center gap-4 p-8">
      <h1 className="text-2xl font-semibold tracking-tight">Paradigm</h1>
      <p className="text-muted-foreground max-w-md text-center text-sm">
        Foundation stack: shadcn/ui, Tailwind CSS, TanStack Query, and
        multi-window Tauri.
      </p>
      <div className="flex flex-col items-center gap-2">
        <Button disabled={!isSuccess} variant={isError ? "destructive" : "default"}>
          {isSuccess
            ? "Query client ready"
            : isError
              ? "Connection failed"
              : "Initializing..."}
        </Button>
        <Button
          variant="outline"
          disabled={isOpeningWindow}
          onClick={() => void handleOpenProofWindow()}
        >
          {isOpeningWindow ? "Opening..." : "Open proof window"}
        </Button>
      </div>
      {windowError ? (
        <p className="text-destructive max-w-md text-center text-sm">
          {windowError}
        </p>
      ) : null}
    </main>
  );
}

function App() {
  const windowLabel = useWindowLabel();

  if (isProofWindow(windowLabel)) {
    return <ProofWindowView />;
  }

  return <MainWindowView />;
}

export default App;
