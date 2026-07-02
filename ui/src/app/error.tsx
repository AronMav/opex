"use client";

import { Button } from "@/components/ui/button";
import { ErrorState } from "@/components/ui/error-state";
import { useTranslation } from "@/hooks/use-translation";

export default function RootError({
  error,
  reset,
}: {
  error: Error & { digest?: string };
  reset: () => void;
}) {
  const { t } = useTranslation();
  return (
    <ErrorState
      className="min-h-dvh"
      message={error.message}
      action={
        <Button onClick={reset} variant="outline" size="sm">
          {t("error.retry")}
        </Button>
      }
    />
  );
}
