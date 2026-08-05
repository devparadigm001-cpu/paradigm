import { useEffect, useState } from "react";
import { getWindowLabel } from "@/lib/windows";

export function useWindowLabel() {
  const [label, setLabel] = useState<string | null>(() => getWindowLabel());

  useEffect(() => {
    setLabel(getWindowLabel());
  }, []);

  return label;
}
